// The whitewater stage's fused kernels. Phase functions are copies of the named atom bodies.
// No hoisting: the atom pipelines remain the bitwise oracle under Fast math.
// Ported from FLIP Fluids (MIT); see THIRD_PARTY_NOTICES.md.
struct Element { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32, }
struct KnownValue { value: f32, known: u32, }
struct FaceSample { face_velocity: vec4<f32>, face_weight: vec4<f32>, }
struct WhitewaterSource { influence: f32, dust_strength: f32, kind: u32, pad: u32, }
struct EmitParams {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    cell_size: f32,
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
    live_count: f32,
    dt: f32,
    dust_enabled: f32,
    boundary_dust: f32,
    count: u32,
    _pad: vec2<u32>,
}
@group(0) @binding(0) var<uniform> p: EmitParams;
@group(0) @binding(1) var<storage, read> buf_particles: array<Element>;
@group(0) @binding(2) var<storage, read> buf_face_u: array<f32>;
@group(0) @binding(3) var<storage, read> buf_face_v: array<f32>;
@group(0) @binding(4) var<storage, read> buf_face_w: array<f32>;
@group(0) @binding(5) var<storage, read> buf_distance: array<f32>;
@group(0) @binding(6) var<storage, read> buf_cells: array<u32>;
@group(0) @binding(7) var<storage, read> buf_curvature: array<KnownValue>;
@group(0) @binding(8) var<storage, read> buf_turbulence: array<f32>;
@group(0) @binding(9) var<storage, read> buf_influence: array<f32>;
@group(0) @binding(10) var<storage, read_write> out_sampled: array<Element>;
@group(0) @binding(11) var<storage, read_write> out_energy: array<f32>;
@group(0) @binding(12) var<storage, read_write> out_counts: array<u32>;
@group(0) @binding(13) var<storage, read_write> out_unscaled: array<Element>;
@group(0) @binding(14) var<storage, read_write> out_wavecrest_bits: array<u32>;
// Dust has disjoint entry-point resources; its solid/source occupy the face slots.
@group(0) @binding(2) var<storage, read> buf_solid: array<f32>;
@group(0) @binding(3) var<storage, read> buf_source: array<WhitewaterSource>;
fn sf_face_len(axis: u32) -> u32 {
    if axis == 0u {
        return arrayLength(&buf_face_u);
    }
    if axis == 1u {
        return arrayLength(&buf_face_v);
    }
    return arrayLength(&buf_face_w);
}

fn sf_face(axis: u32, i: u32) -> f32 {
    if axis == 0u {
        return buf_face_u[i];
    }
    if axis == 1u {
        return buf_face_v[i];
    }
    return buf_face_w[i];
}

fn wc_distance(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_distance[ww_cell_index(vec3<u32>(c), cells)];
}

fn wc_borders_air(c: vec3<i32>, cells: vec3<u32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let n = c + vec3<i32>(dx, dy, dz);
                if (dx == 0 && dy == 0 && dz == 0) || !ww_in_grid(n, cells) {
                    continue;
                }
                if buf_cells[ww_cell_index(vec3<u32>(n), cells)] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
}

fn wc_curvature(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_curvature[ww_cell_index(vec3<u32>(c), cells)].value;
}

fn wc_turbulence(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_turbulence[ww_cell_index(vec3<u32>(c), cells)];
}

fn dust_solid(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_solid[ww_cell_index(vec3<u32>(c), cells)];
}

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

fn ww_phase_sample(
    idx: u32,
    count: u32,
    e_particles: Element,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
) -> Element {
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
    }
    let still = Element(e_particles.position_radius, vec3<f32>(0.0), e_particles.id);
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let face_cells = vec3<u32>(max(round(vec3<f32>(face_cells_x, face_cells_y, face_cells_z)), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(face_cells == vec3<u32>(0u)) {
        return still;
    }
    let cells = nodes - vec3<u32>(1u);
    if any(face_cells > cells) {
        return still;
    }
    let q = ww_grid_position(
        e_particles.position_radius.xyz,
        vec3<f32>(center_x, center_y, center_z),
        vec3<f32>(size_x, size_y, size_z),
        cells,
    );
    if any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) {
        return still;
    }
    let pad = lf_pad(cells, face_cells);
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        let len = sf_face_len(axis);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let i = lf_face_index(base + vec3<i32>(ww_corner(corner)), axis, pad, face_cells);
            if i != LF_NONE && i < len {
                sum = sum + ww_corner_weight(f, corner) * sf_face(axis, i);
            }
        }
        v[axis] = sum;
    }
    return Element(e_particles.position_radius, v, e_particles.id);
}

fn ww_phase_velocity(
    idx: u32,
    count: u32,
    e_particles: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    spray_speed: f32,
    seed: f32,
    epoch: f32,
) -> Element {
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return e_particles;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if total > arrayLength(&buf_distance) || total > arrayLength(&buf_cells) {
        return e_particles;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(e_particles.position_radius.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        phi[corner] = wc_distance(c, cells);
        let w = ww_corner_weight(f, corner);
        d = d + w * phi[corner];
    }
    var out = e_particles;
    if abs(d) < 1.5 * h && wc_borders_air(vec3<i32>(floor(q)), cells) && d > -0.75 * h {
        out.velocity *= 1.0 + (spray_speed - 1.0) * ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 9u);
    }
    return out;
}

fn ww_phase_energy(idx: u32, count: u32, e_particles: Element, min_energy: f32, max_energy: f32) -> f32 {
    if !(e_particles.position_radius.w > 0.0) || !(max_energy > min_energy) {
        return 0.0;
    }
    let v = e_particles.velocity;
    let e = min(max(0.5 * dot(v, v), min_energy), max_energy);
    return (e - min_energy) / (max_energy - min_energy);
}

fn ww_phase_wavecrest(
    idx: u32,
    count: u32,
    e_particles: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    min_curvature: f32,
    max_curvature: f32,
    sharpness: f32,
) -> f32 {
    if !(e_particles.position_radius.w > 0.0) || !(max_curvature > min_curvature) {
        return 0.0;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return 0.0;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if total > arrayLength(&buf_distance) || total > arrayLength(&buf_curvature) || total > arrayLength(&buf_cells) {
        return 0.0;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(e_particles.position_radius.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    var d = 0.0;
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        phi[corner] = wc_distance(c, cells);
        let w = ww_corner_weight(f, corner);
        d = d + w * phi[corner];
        k = k + w * wc_curvature(c, cells);
    }
    if !(abs(d) < 1.5 * h) || !wc_borders_air(vec3<i32>(floor(q)), cells) {
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

fn ww_phase_inside(
    idx: u32,
    count: u32,
    e_particles: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    min_turbulence: f32,
    max_turbulence: f32,
    inside_enabled: f32,
) -> f32 {
    if !(e_particles.position_radius.w > 0.0) || !(max_turbulence > min_turbulence) {
        return 0.0;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return 0.0;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if total > arrayLength(&buf_distance) || total > arrayLength(&buf_turbulence) || total > arrayLength(&buf_cells) {
        return 0.0;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(e_particles.position_radius.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    var d = 0.0;
    var k = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let c = base + vec3<i32>(ww_corner(corner));
        phi[corner] = wc_distance(c, cells);
        let w = ww_corner_weight(f, corner);
        d = d + w * phi[corner];
        k = k + w * wc_turbulence(c, cells);
    }
    if (abs(d) < 1.5 * h) && wc_borders_air(vec3<i32>(floor(q)), cells) {
        return 0.0;
    }
    return inside_enabled * (clamp(k, min_turbulence, max_turbulence) - min_turbulence) / (max_turbulence - min_turbulence);
}

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
    center_x: f32, center_y: f32, center_z: f32,
    size_x: f32, size_y: f32, size_z: f32,
    nodes_x: f32, nodes_y: f32, nodes_z: f32,
) -> u32 {
    if f32(idx) >= live_count || !(e_particles.position_radius.w > 0.0) || !(points_per_cell > 0.0) {
        return 0u;
    }
    if length(e_particles.velocity) < 1e-3 || e_energy < 1e-6 {
        return 0u;
    }
    if ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 10u) >= generation_rate { return 0u; }
    let nodes = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = nodes - vec3<u32>(1u);
    let q = ww_grid_position(e_particles.position_radius.xyz, vec3<f32>(center_x, center_y, center_z), vec3<f32>(size_x, size_y, size_z), cells);
    if any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) { return 0u; }
    let node = ww_cell_index(vec3<u32>(q), nodes);
    if node >= arrayLength(&buf_influence) { return 0u; }
    let per_tick = buf_influence[node] * e_energy * (rate * e_wavecrest + turbulence_rate * e_turbulence) * dt * 8.0 / points_per_cell;
    if !(per_tick > 0.0) {
        return 0u;
    }
    return u32(floor(per_tick + 0.5)) * u32(max(round(ticks), 0.0));
}

fn ww_phase_dust(
    idx: u32,
    count: u32,
    e_particles: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    min_turbulence: f32,
    max_turbulence: f32,
    dust_enabled: f32, boundary_dust: f32,
) -> f32 {
    if !(e_particles.position_radius.w > 0.0) || !(max_turbulence > min_turbulence) {
        return 0.0;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return 0.0;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if nodes.x * nodes.y * nodes.z > arrayLength(&buf_solid) || total > arrayLength(&buf_turbulence) || nodes.x * nodes.y * nodes.z > arrayLength(&buf_source) {
        return 0.0;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(e_particles.position_radius.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    if dust_enabled < 0.5 || any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) { return 0.0; }
    let source = buf_source[ww_cell_index(vec3<u32>(q), nodes)];
    if source.kind == 0u || source.dust_strength <= 0.0 { return 0.0; }
    if source.kind == 1u && (boundary_dust < 0.5 || q.z > 3.0) { return 0.0; }
    let solid_base = vec3<i32>(floor(q));
    var clearance = 0.0;
    for (var corner = 0u; corner < 8u; corner++) {
        clearance += ww_corner_weight(fract(q), corner) * dust_solid(solid_base + vec3<i32>(ww_corner(corner)), nodes);
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
        k = k + w * wc_turbulence(c, cells);
    }
    let minimum = 0.75 * min_turbulence;
    return source.dust_strength * (clamp(k, minimum, max_turbulence) - minimum) / (max_turbulence - minimum);
}
@compute @workgroup_size(256)
fn ww_emit(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= p.count { return; }
    let jittered = ww_phase_jitter(idx, p.count, buf_particles[idx], p.cell_size, p.seed, p.epoch);
    let unscaled = ww_phase_sample(idx, p.count, jittered, p.face_cells_x, p.face_cells_y, p.face_cells_z, p.center_x, p.center_y, p.center_z, p.size_x, p.size_y, p.size_z, p.nodes_x, p.nodes_y, p.nodes_z);
    let sampled = ww_phase_velocity(idx, p.count, unscaled, p.center_x, p.center_y, p.center_z, p.size_x, p.size_y, p.size_z, p.nodes_x, p.nodes_y, p.nodes_z, p.spray_speed, p.seed, p.epoch);
    let energy = ww_phase_energy(idx, p.count, sampled, p.min_energy, p.max_energy);
    let wavecrest = ww_phase_wavecrest(idx, p.count, sampled, p.center_x, p.center_y, p.center_z, p.size_x, p.size_y, p.size_z, p.nodes_x, p.nodes_y, p.nodes_z, p.min_curvature, p.max_curvature, p.sharpness);
    let inside = ww_phase_inside(idx, p.count, sampled, p.center_x, p.center_y, p.center_z, p.size_x, p.size_y, p.size_z, p.nodes_x, p.nodes_y, p.nodes_z, p.min_turbulence, p.max_turbulence, p.inside_enabled);
    let counts = ww_phase_count(idx, p.count, sampled, energy, wavecrest, inside, p.rate, p.turbulence_rate, p.generation_rate, p.seed, p.epoch, p.points_per_cell, p.ticks, p.live_count, p.dt, p.center_x, p.center_y, p.center_z, p.size_x, p.size_y, p.size_z, p.nodes_x, p.nodes_y, p.nodes_z);
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
    if idx >= p.count { return; }
    let unscaled = buf_particles[idx];
    let dust = ww_phase_dust(idx, p.count, unscaled, p.center_x, p.center_y, p.center_z, p.size_x, p.size_y, p.size_z, p.nodes_x, p.nodes_y, p.nodes_z, p.min_turbulence, p.max_turbulence, p.dust_enabled, p.boundary_dust);
    let energy = ww_phase_energy(idx, p.count, unscaled, p.min_energy, p.max_energy);
    let counts = ww_phase_count(idx, p.count, unscaled, energy, bitcast<f32>(out_wavecrest_bits[idx]), dust, p.rate, p.turbulence_rate, p.generation_rate, p.seed, p.epoch, p.points_per_cell, p.ticks, p.live_count, p.dt, p.center_x, p.center_y, p.center_z, p.size_x, p.size_y, p.size_z, p.nodes_x, p.nodes_y, p.nodes_z);
    out_energy[idx] = energy;
    out_counts[idx] = counts;
}

// Resources are disjoint per entry point, retaining the 16-binding dispatch cap.
struct Spawn { position_lifetime: vec4<f32>, velocity: vec3<f32>, kind: u32, }
struct Pool { position_lifetime: vec4<f32>, velocity: vec3<f32>, kind: u32, id: u32, _pad0: u32, _pad1: u32, _pad2: u32, }
struct SpawnParams {
    capacity: f32,
    emitters: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
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
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}
struct LifecycleParams {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
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
    bubble_lifetime_modifier: f32,
    foam_lifetime_modifier: f32,
    spray_lifetime_modifier: f32,
    count: u32,
    _pad0: u32,
}
@group(0) @binding(0) var<uniform> sp: SpawnParams;
@group(0) @binding(0) var<uniform> lc: LifecycleParams;
@group(0) @binding(7) var<storage, read> buf_offsets: array<u32>;
@group(0) @binding(8) var<storage, read> buf_energy: array<f32>;
@group(0) @binding(9) var<storage, read> buf_spawn_solid: array<f32>;
@group(0) @binding(10) var<storage, read_write> out_typed: array<Spawn>;
@group(0) @binding(1) var<storage, read> buf_pool: array<Pool>;
@group(0) @binding(7) var<storage, read> buf_lifecycle_solid: array<f32>;
@group(0) @binding(8) var<storage, read> buf_substep_schedule: array<f32>;
@group(0) @binding(9) var<storage, read> buf_substep_u: array<f32>;
@group(0) @binding(10) var<storage, read> buf_substep_v: array<f32>;
@group(0) @binding(11) var<storage, read> buf_substep_w: array<f32>;
@group(0) @binding(12) var<storage, read> buf_forces: array<f32>;
@group(0) @binding(13) var<storage, read> buf_impulses: array<f32>;
@group(0) @binding(14) var<storage, read_write> out_pool: array<Pool>;

// node.spawn_whitewater — fusable BUFFER body, GATHER only. One thread per
// spawn slot j, FLIP's _emitDiffuseParticles (diffuseparticlesimulation.cpp
// :1912) for one new particle:
//   the frame emits total = offsets[emitters − 1]; slot j < min(total,
//   capacity) takes emission m = j, or ⌊j · total / capacity⌋ past capacity
//   (a uniform subset), and m's emitter e is the first with offsets[e] > m;
//   the particle lands in a cylinder about e's velocity: radius 8 marker
//   radii · √Xr, angle 2π·Xt, height Xh · |v| · dt along it (export supplies 1/60 s);
//   it is dropped outside the grid, or where the solid lattice's distance is
//   under a quarter cell;
//   lifetime = min + Ie·(max − min) ± variance, dropped at or below 0;
//   velocity is FLIP's MAC trilinear of the faces at its position.
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

// floor(a·b / c) for a < c, through the 64-bit product.
fn sw_mul_div(a: u32, b: u32, c: u32) -> u32 {
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

fn sw_face_len(axis: u32) -> u32 {
    if axis == 0u {
        return arrayLength(&buf_face_u);
    }
    if axis == 1u {
        return arrayLength(&buf_face_v);
    }
    return arrayLength(&buf_face_w);
}

fn sw_face(axis: u32, i: u32) -> f32 {
    if axis == 0u {
        return buf_face_u[i];
    }
    if axis == 1u {
        return buf_face_v[i];
    }
    return buf_face_w[i];
}

// FLIP's MAC trilinear at q (whitewater cells), a face past the face grid
// reading 0; q lies inside the grid.
fn sw_velocity(q: vec3<f32>, cells: vec3<u32>, face_cells: vec3<u32>) -> vec3<f32> {
    let pad = lf_pad(cells, face_cells);
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        let len = sw_face_len(axis);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let i = lf_face_index(base + vec3<i32>(ww_corner(corner)), axis, pad, face_cells);
            if i != LF_NONE && i < len {
                sum = sum + ww_corner_weight(f, corner) * sw_face(axis, i);
            }
        }
        v[axis] = sum;
    }
    return v;
}

// The solid lattice's distance at q, trilinear over its nodes (a node past
// the lattice reads 0), metres.
fn sw_solid(q: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let n = base + vec3<i32>(ww_corner(corner));
        if all(n >= vec3<i32>(0)) && all(n < vec3<i32>(nodes)) {
            let u = vec3<u32>(n);
            d = d + ww_corner_weight(f, corner) * buf_spawn_solid[u.x + nodes.x * (u.y + nodes.y * u.z)];
        }
    }
    return d;
}

fn ww_phase_spawn(
    idx: u32,
    count: u32,
    capacity: f32,
    emitters: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    seed: f32,
    epoch: f32,
    min_lifetime: f32,
    max_lifetime: f32,
    lifetime_variance: f32,
    dt: f32,
) -> Spawn {
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
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let face_cells = vec3<u32>(max(round(vec3<f32>(face_cells_x, face_cells_y, face_cells_z)), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(face_cells == vec3<u32>(0u)) || nodes.x * nodes.y * nodes.z > arrayLength(&buf_spawn_solid) {
        return empty;
    }
    let cells = nodes - vec3<u32>(1u);
    if any(face_cells > cells) {
        return empty;
    }
    let emitter = buf_particles[e];
    let v = emitter.velocity;
    if !(emitter.position_radius.w > 0.0) || length(v) < 1e-3 {
        return empty;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
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
    let q = ww_grid_position(p, vec3<f32>(center_x, center_y, center_z), size, cells);
    if !ww_in_grid(vec3<i32>(floor(q)), cells) {
        return empty;
    }
    if sw_solid(q, nodes) < SW_SOLID_BUFFER * h {
        return empty;
    }
    var lifetime = min_lifetime + buf_energy[e] * (max_lifetime - min_lifetime);
    lifetime = lifetime + lifetime_variance * (2.0 * ww_random(idx, s, generation, 7u) - 1.0);
    if !(lifetime > 0.0) {
        return empty;
    }
    return Spawn(vec4<f32>(p, lifetime), sw_velocity(q, cells, face_cells), 0u);
}

// node.whitewater_type — fusable BUFFER body, COINCIDENT spawns, GATHER
// distance and cells. FLIP's _getDiffuseParticleType
// (diffuseparticlesimulation.cpp:2056) for a fresh particle, with FLIP's
// defaults:
//   spray (2) outside the boundary box, which sits 1.625 cells inside the
//   grid (FLIP's box 3 cells smaller than the domain, then a quarter cell,
//   AABB::expand moving each side by half);
//   else by the distance at the particle, read trilinearly at cell centres
//   (a cell outside the grid reads 0): foam (1) within a cell of the
//   surface, bubble (0) deeper, spray (2) higher;
//   foam or spray whose cell borders no air (26 neighbours, outside the grid
//   counting as solid) becomes bubble.
// Slots with lifetime 0 pass whole. The lifecycle types every particle by
// the same rule on each step, so the two must agree.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's _maxFoamToSurfaceDistance and _foamLayerOffset, cells.
const WT_FOAM_DEPTH: f32 = 1.0;
const WT_FOAM_OFFSET: f32 = 0.0;
// FLIP's boundary box inset, cells, and its 1e-6 m epsilon halved.
const WT_BOX_INSET: f32 = 1.625;
const WT_BOX_EPSILON: f32 = 0.5e-6;

fn wt_distance(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_distance[ww_cell_index(vec3<u32>(c), cells)];
}

fn wt_borders_air(c: vec3<i32>, cells: vec3<u32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let n = c + vec3<i32>(dx, dy, dz);
                if (dx == 0 && dy == 0 && dz == 0) || !ww_in_grid(n, cells) {
                    continue;
                }
                if buf_cells[ww_cell_index(vec3<u32>(n), cells)] == 0u {
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
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    spray_speed: f32, seed: f32, epoch: f32, dust: f32,
) -> Spawn {
    var spawn = e_spawns;
    if !(spawn.position_lifetime.w > 0.0) {
        return spawn;
    }
    if dust > 0.5 { spawn.kind = 4u; return spawn; }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return spawn;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if total > arrayLength(&buf_distance) || total > arrayLength(&buf_cells) {
        return spawn;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(spawn.position_lifetime.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    let lo = vec3<f32>(WT_BOX_INSET + WT_BOX_EPSILON / h);
    let hi = vec3<f32>(cells) - lo;
    if any(q < lo) || any(q >= hi) {
        spawn.kind = 2u;
        return wt_finish(spawn, idx, spray_speed, seed, epoch);
    }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * wt_distance(base + vec3<i32>(ww_corner(corner)), cells);
    }
    let depth = WT_FOAM_DEPTH * h;
    let offset = WT_FOAM_OFFSET * h;
    var kind = 2u;
    if d > -depth + offset && d < depth + offset {
        kind = 1u;
    } else if d < -depth + offset {
        kind = 0u;
    }
    if kind != 0u && !wt_borders_air(vec3<i32>(floor(q)), cells) {
        kind = 0u;
    }
    spawn.kind = kind;
    return wt_finish(spawn, idx, spray_speed, seed, epoch);
}

// node.advect_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER faces
// and solid. One FLIP tick of a live whitewater particle by its type
// (diffuseparticlesimulation.cpp:2250-2600), every limit behaviour collide:
//   spray: gravity and per-id drag, then the collision march; a collision
//     with a usable normal sets velocity from the old one split along it
//     (friction on the tangent part, restitution on the normal part) plus
//     gravity;
//   bubble: buoyancy against gravity and drag toward the liquid velocity;
//   foam: the liquid velocity times the advection strength;
//   all: a particle that moved faster than 1.1 times its new speed dies
//     (lifetime -1e6), and so does one whose travel is not finite.
// Work is in FLIP's local frame: metres from the grid's first node. The
// liquid velocity is FLIP's MAC trilinear at the old position (0 outside
// the grid), the solid its node lattice read trilinearly (a node past the
// lattice reads 0). FLIP's near-solid early-out is dropped
// (GPU_WHITEWATER_DESIGN.md section 3.9); its range check stays. Slots with
// kind 3 is empty; kind 4 is dust, with FLIP's ID-dependent buoyancy and drag.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's boundary box: 3 cells and 1e-6 m in from the domain, then
// _solidBufferWidth (a quarter cell) more, each side moving by half.
const AW_BOX_INSET: f32 = 1.625;
const AW_BOX_EPSILON: f32 = 0.5e-6;
// _solidBufferWidth, _diffuseParticleStepDistanceFactor and the CFL number,
// cells; _maxVelocityFactor.
const AW_SOLID_BUFFER: f32 = 0.25;
const AW_STEP: f32 = 0.5;
const AW_MAX_RESOLVE: f32 = 5.0;
const AW_MAX_VELOCITY: f32 = 1.1;
// _nearSolidGridCellSizeFactor, cells.
const AW_NEAR_SOLID: f32 = 3.0;
const AW_EPS: f32 = 1e-6;
const AW_DEAD: f32 = -1e6;
// _diffuseParticleIDLimit - 1.
const AW_ID_TOP: f32 = 255.0;

fn aw_face_len(axis: u32) -> u32 {
    if axis == 0u {
        return arrayLength(&buf_face_u);
    }
    if axis == 1u {
        return arrayLength(&buf_face_v);
    }
    return arrayLength(&buf_face_w);
}

fn aw_face(axis: u32, i: u32, step: u32, cells: vec3<u32>) -> f32 {
    if step != 0xffffffffu {
        var dims = cells; dims[axis] = dims[axis] + 1u;
        let at = step * dims.x * dims.y * dims.z + i;
        if axis == 0u { return buf_substep_u[at]; }
        if axis == 1u { return buf_substep_v[at]; }
        return buf_substep_w[at];
    }
    if axis == 0u {
        return buf_face_u[i];
    }
    if axis == 1u {
        return buf_face_v[i];
    }
    return buf_face_w[i];
}

// FLIP's MAC trilinear at grid position q.
fn aw_velocity(q: vec3<f32>, cells: vec3<u32>, face_cells: vec3<u32>, step: u32) -> vec3<f32> {
    if any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) {
        return vec3<f32>(0.0);
    }
    let pad = lf_pad(cells, face_cells);
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        let len = aw_face_len(axis);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let i = lf_face_index(base + vec3<i32>(ww_corner(corner)), axis, pad, face_cells);
            if i != LF_NONE && i < len {
                sum = sum + ww_corner_weight(f, corner) * aw_face(axis, i, step, face_cells);
            }
        }
        v[axis] = sum;
    }
    return v;
}

fn aw_node(n: vec3<i32>, nodes: vec3<u32>) -> f32 {
    if any(n < vec3<i32>(0)) || any(n >= vec3<i32>(nodes)) {
        return 0.0;
    }
    let u = vec3<u32>(n);
    return buf_lifecycle_solid[u.x + nodes.x * (u.y + nodes.y * u.z)];
}

// The solid's distance at grid position q, metres.
fn aw_solid(q: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * aw_node(base + vec3<i32>(ww_corner(corner)), nodes);
    }
    return d;
}

// FLIP's trilinear gradient (interpolation.cpp:197), unscaled; corner index
// x + 2y + 4z.
fn aw_gradient(q: vec3<f32>, nodes: vec3<u32>) -> vec3<f32> {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        phi[corner] = aw_node(base + vec3<i32>(ww_corner(corner)), nodes);
    }
    let gx = mix(mix(phi[1] - phi[0], phi[3] - phi[2], f.y), mix(phi[5] - phi[4], phi[7] - phi[6], f.y), f.z);
    let gy = mix(mix(phi[2] - phi[0], phi[3] - phi[1], f.x), mix(phi[6] - phi[4], phi[7] - phi[5], f.x), f.z);
    let gz = mix(mix(phi[4] - phi[0], phi[5] - phi[1], f.x), mix(phi[6] - phi[2], phi[7] - phi[3], f.x), f.y);
    return vec3<f32>(gx, gy, gz);
}

fn aw_inside(p: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>) -> bool {
    return all(p >= lo) && all(p < hi);
}

// Whether a local position's near-solid cell lies in FLIP's near-solid grid.
fn aw_in_near_solid(p: vec3<f32>, h: f32, cells: vec3<u32>) -> bool {
    let g = floor(p / (AW_NEAR_SOLID * h));
    let top = vec3<f32>((cells + vec3<u32>(2u)) / vec3<u32>(3u));
    return all(g >= vec3<f32>(0.0)) && all(g < top);
}

fn aw_step(
    idx: u32,
    count: u32,
    e_pool: Pool,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
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
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let face_cells = vec3<u32>(max(round(vec3<f32>(face_cells_x, face_cells_y, face_cells_z)), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(face_cells == vec3<u32>(0u)) || nodes.x * nodes.y * nodes.z > arrayLength(&buf_lifecycle_solid) {
        return out;
    }
    let cells = nodes - vec3<u32>(1u);
    if any(face_cells > cells) {
        return out;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let origin = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let p = e_pool.position_lifetime.xyz - origin;
    let v = e_pool.velocity;
    let gravity = vec3<f32>(gravity_x, gravity_y, gravity_z);
    let lo = vec3<f32>(AW_BOX_INSET * h + AW_BOX_EPSILON);
    let hi = vec3<f32>(cells) * h - lo;
    let spray = e_pool.kind == 2u;

    var nextv: vec3<f32>;
    if spray {
        let factor = f32(e_pool.id) / AW_ID_TOP;
        let mind = max(spray_drag - spray_drag * spray_drag_variance, 0.0);
        let maxd = spray_drag + spray_drag * spray_drag_variance;
        let drag = mind + (1.0 - factor) * (maxd - mind);
        nextv = v + gravity * dt + (-drag * v * dt);
    } else {
        let vmac = aw_velocity(p / h, cells, face_cells, face_step);
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
    if aw_in_near_solid(p, h, cells) && aw_in_near_solid(nextp, h, cells) && travel >= AW_EPS {
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
            let phi = aw_solid(current / h, nodes);
            if phi < 0.0 || !aw_inside(current, lo, hi) {
                hit_phi = phi;
                found = true;
                break;
            }
            last = current;
        }
        if found {
            let reach = AW_MAX_RESOLVE * h;
            let grad = aw_gradient(current / h, nodes);
            if length(grad) > AW_EPS {
                let n = normalize(grad);
                resolved = current - (hit_phi - AW_SOLID_BUFFER * h) * n;
                if aw_solid(resolved / h, nodes) < 0.0 || length(resolved - current) > reach {
                    resolved = last;
                }
                let u = dot(v, n) * n;
                bounce = (1.0 - spray_friction) * (v - u) - spray_restitution * u;
                bounced = true;
            } else {
                resolved = last;
            }
            if !aw_inside(resolved, lo, hi) {
                let before = resolved;
                resolved = min(max(resolved, lo), hi - vec3<f32>(AW_EPS));
                if aw_solid(resolved / h, nodes) < 0.0 || length(resolved - before) > reach {
                    resolved = last;
                }
            }
        }
    }
    if spray && bounced {
        nextv = bounce + gravity * dt;
    }
    if length(resolved - p) * (1.0 / dt) > AW_MAX_VELOCITY * length(nextv) {
        out.position_lifetime.w = AW_DEAD;
    }
    out.position_lifetime = vec4<f32>(resolved + origin, out.position_lifetime.w);
    out.velocity = nextv;
    return out;
}

fn ww_phase_advect(
    idx: u32,
    count: u32,
    e_pool: Pool,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
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
) -> Pool {
    let steps = u32(max(substep_count, 0.0));
    let origin = vec3<f32>(center_x,center_y,center_z) - 0.5 * vec3<f32>(size_x,size_y,size_z);
    let h = size_x / (nodes_x - 1.0);
    let padding = 0.5 * (nodes_x - 1.0 - face_cells_x);
    let field_origin = origin + vec3<f32>(padding * h);
    let dims = vec3<u32>(vec3<f32>(field_nodes_x,field_nodes_y,field_nodes_z));
    let stride = dims.x * dims.y * dims.z * 4u;
    let base = liquid_field_force_base(i32(tick_index), i32(first_tick), i32(force_lattices), dims);
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
            let c = liquid_field_corner(particle.position_lifetime.xyz, field_origin, field_spacing, dims, corner);
            for (var a = 0u; a < 3u; a = a + 1u) {
                if force_lattices > 0.0 {
                    acceleration[a] = fma(buf_forces[base + c.index * 4u + a], c.weight, acceleration[a]);
                }
                if (event & 0x80000000u) != 0u {
                    let at = (event & 0x7fffffffu) * stride + c.index * 4u + a;
                    if at < arrayLength(&buf_impulses) { impulse[a] = fma(buf_impulses[at], c.weight, impulse[a]); }
                }
            }
        }
        // Foam gets the impulse through the already-forced liquid velocity.
        if particle.kind != 1u && (event & 0x80000000u) != 0u { particle.velocity = particle.velocity + impulse; }
        particle = aw_step(idx, count, particle, center_x, center_y, center_z, size_x, size_y, size_z, nodes_x, nodes_y, nodes_z, face_cells_x, face_cells_y, face_cells_z, acceleration.x, acceleration.y, acceleration.z, duration, foam_advection, bubble_buoyancy, bubble_drag, spray_drag, spray_drag_variance, spray_restitution, spray_friction, face_step);
    }
    return particle;
}

// node.retype_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER
// distance, cells and faces. FLIP's _updateDiffuseParticleTypes
// (diffuseparticlesimulation.cpp:2033) after each advect, with FLIP's
// defaults: the same rule node.whitewater_type gives a fresh particle
// (spray outside the boundary box; else foam within a cell of the surface,
// bubble deeper, spray higher; foam or spray away from air becomes bubble),
// except that foam turning bubble stays foam until it sinks a further cell
// (_foamBufferWidth). A bubble that turns foam or spray takes the liquid
// velocity at its position (FLIP's MAC trilinear, 0 outside the grid).
// Dust (kind 4) and empty slots (kind 3) pass whole; a dead particle is still retyped, as FLIP retypes it before removal.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's _maxFoamToSurfaceDistance, _foamLayerOffset and _foamBufferWidth,
// cells.
const RT_FOAM_DEPTH: f32 = 1.0;
const RT_FOAM_OFFSET: f32 = 0.0;
const RT_FOAM_BUFFER: f32 = 1.0;
// FLIP's boundary box inset, cells, and its 1e-6 m epsilon halved.
const RT_BOX_INSET: f32 = 1.625;
const RT_BOX_EPSILON: f32 = 0.5e-6;

fn rt_distance(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_distance[ww_cell_index(vec3<u32>(c), cells)];
}

fn rt_borders_air(c: vec3<i32>, cells: vec3<u32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let n = c + vec3<i32>(dx, dy, dz);
                if (dx == 0 && dy == 0 && dz == 0) || !ww_in_grid(n, cells) {
                    continue;
                }
                if buf_cells[ww_cell_index(vec3<u32>(n), cells)] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
}

fn rt_face_len(axis: u32) -> u32 {
    if axis == 0u {
        return arrayLength(&buf_face_u);
    }
    if axis == 1u {
        return arrayLength(&buf_face_v);
    }
    return arrayLength(&buf_face_w);
}

fn rt_face(axis: u32, i: u32) -> f32 {
    if axis == 0u {
        return buf_face_u[i];
    }
    if axis == 1u {
        return buf_face_v[i];
    }
    return buf_face_w[i];
}

// FLIP's MAC trilinear at grid position q.
fn rt_velocity(q: vec3<f32>, cells: vec3<u32>, face_cells: vec3<u32>) -> vec3<f32> {
    if any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) {
        return vec3<f32>(0.0);
    }
    let pad = lf_pad(cells, face_cells);
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        let len = rt_face_len(axis);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let i = lf_face_index(base + vec3<i32>(ww_corner(corner)), axis, pad, face_cells);
            if i != LF_NONE && i < len {
                sum = sum + ww_corner_weight(f, corner) * rt_face(axis, i);
            }
        }
        v[axis] = sum;
    }
    return v;
}

fn rt_kind(q: vec3<f32>, old: u32, h: f32, cells: vec3<u32>) -> u32 {
    let lo = vec3<f32>(RT_BOX_INSET + RT_BOX_EPSILON / h);
    let hi = vec3<f32>(cells) - lo;
    if any(q < lo) || any(q >= hi) {
        return 2u;
    }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * rt_distance(base + vec3<i32>(ww_corner(corner)), cells);
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
    if kind != 0u && !rt_borders_air(vec3<i32>(floor(q)), cells) {
        kind = 0u;
    }
    return kind;
}

fn ww_phase_retype(
    idx: u32,
    count: u32,
    e_pool: Pool,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
) -> Pool {
    var out = e_pool;
    if e_pool.kind > 2u {
        return out;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let face_cells = vec3<u32>(max(round(vec3<f32>(face_cells_x, face_cells_y, face_cells_z)), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(face_cells == vec3<u32>(0u)) {
        return out;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if any(face_cells > cells) || total > arrayLength(&buf_distance) || total > arrayLength(&buf_cells) {
        return out;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(e_pool.position_lifetime.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    let kind = rt_kind(q, e_pool.kind, h, cells);
    if e_pool.kind == 0u && kind != 0u {
        out.velocity = rt_velocity(q, cells, face_cells);
    }
    out.kind = kind;
    return out;
}

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

@compute @workgroup_size(256)
fn ww_spawn(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= sp.count { return; }
    let spawn = ww_phase_spawn(idx, sp.count, sp.capacity, sp.emitters, sp.face_cells_x, sp.face_cells_y, sp.face_cells_z, sp.center_x, sp.center_y, sp.center_z, sp.size_x, sp.size_y, sp.size_z, sp.nodes_x, sp.nodes_y, sp.nodes_z, sp.seed, sp.epoch, sp.min_lifetime, sp.max_lifetime, sp.lifetime_variance, sp.dt);
    let typed = ww_phase_type(idx, sp.count, spawn, sp.center_x, sp.center_y, sp.center_z, sp.size_x, sp.size_y, sp.size_z, sp.nodes_x, sp.nodes_y, sp.nodes_z, sp.spray_speed, sp.type_seed, sp.type_epoch, sp.dust);
    out_typed[idx] = typed;
}

@compute @workgroup_size(256)
fn ww_lifecycle(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= lc.count { return; }
    let advected = ww_phase_advect(idx, lc.count, buf_pool[idx], lc.center_x, lc.center_y, lc.center_z, lc.size_x, lc.size_y, lc.size_z, lc.nodes_x, lc.nodes_y, lc.nodes_z, lc.face_cells_x, lc.face_cells_y, lc.face_cells_z, lc.gravity_x, lc.gravity_y, lc.gravity_z, lc.dt, lc.foam_advection, lc.bubble_buoyancy, lc.bubble_drag, lc.spray_drag, lc.spray_drag_variance, lc.spray_restitution, lc.spray_friction, lc.substep_count, lc.field_nodes_x, lc.field_nodes_y, lc.field_nodes_z, lc.field_spacing, lc.force_lattices, lc.tick_index, lc.first_tick);
    let retyped = ww_phase_retype(idx, lc.count, advected, lc.center_x, lc.center_y, lc.center_z, lc.size_x, lc.size_y, lc.size_z, lc.nodes_x, lc.nodes_y, lc.nodes_z, lc.face_cells_x, lc.face_cells_y, lc.face_cells_z);
    let aged = ww_phase_age(idx, lc.count, retyped, lc.dt, lc.bubble_lifetime_modifier, lc.foam_lifetime_modifier, lc.spray_lifetime_modifier);
    out_pool[idx] = aged;
}

struct TurbulenceParams {
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    count: u32,
}
@group(0) @binding(0) var<uniform> tf: TurbulenceParams;
@group(0) @binding(6) var<storage, read_write> out_turbulence: array<f32>;
// FLIP Fluids turbulencefield.cpp:100-171 (MIT); THIRD_PARTY_NOTICES.md.
// The asymmetric loop and excluded final boundary index are intentional.
fn tf_face(c: vec3<i32>, axis: u32, pad: vec3<i32>, dims: vec3<u32>) -> f32 {
    let i = lf_face_index(c, axis, pad, dims);
    if i == LF_NONE { return 0.0; }
    if axis == 0u { return buf_face_u[i]; }
    if axis == 1u { return buf_face_v[i]; }
    return buf_face_w[i];
}

fn tf_velocity(c: vec3<i32>, pad: vec3<i32>, dims: vec3<u32>) -> vec3<f32> {
    var v = vec3<f32>(0.0);
    for (var a = 0u; a < 3u; a++) {
        var next = c; next[a]++;
        v[a] = 0.5 * (tf_face(c, a, pad, dims) + tf_face(next, a, pad, dims));
    }
    return v;
}

fn ww_phase_turbulence(idx: u32, count: u32,
    face_cells_x: f32, face_cells_y: f32, face_cells_z: f32,
    nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32,
) -> f32 {
    let cells = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z)) - vec3<u32>(1u);
    if idx >= cells.x * cells.y * cells.z || buf_distance[idx] >= 0.0 { return 0.0; }
    let c = vec3<i32>(i32(idx % cells.x), i32((idx / cells.x) % cells.y), i32(idx / (cells.x * cells.y)));
    let dims = vec3<u32>(vec3<f32>(face_cells_x, face_cells_y, face_cells_z));
    let pad = lf_pad(cells, dims);
    let vi = tf_velocity(c, pad, dims);
    let lo = max(c - vec3<i32>(2), vec3<i32>(0));
    let hi = min(c + vec3<i32>(2), vec3<i32>(cells) - vec3<i32>(1));
    var t = 0.0;
    for (var z = lo.z; z < hi.z; z++) {
        for (var y = lo.y; y < hi.y; y++) {
            for (var x = lo.x; x < hi.x; x++) {
                let n = vec3<i32>(x, y, z);
                let dv = vi - tf_velocity(n, pad, dims);
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
fn ww_turbulence(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= tf.count { return; }
    out_turbulence[idx] = ww_phase_turbulence(idx, tf.count, tf.face_cells_x, tf.face_cells_y, tf.face_cells_z, tf.nodes_x, tf.nodes_y, tf.nodes_z, tf.cell_size);
}

// A dispatch boundary keeps adapter indexing/select out of floating-point
// interpolation. The helper copies face_sample_component's integer indexing,
// select and zero tail, including its dimension casts; no float arithmetic.
struct UnpackParams {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    count: u32,
}
@group(0) @binding(0) var<uniform> unpack: UnpackParams;
@group(0) @binding(1) var<storage, read> buf_faces: array<FaceSample>;
@group(0) @binding(2) var<storage, read_write> unpack_u: array<f32>;
@group(0) @binding(3) var<storage, read_write> unpack_v: array<f32>;
@group(0) @binding(4) var<storage, read_write> unpack_w: array<f32>;
fn ww_unpack_face(idx: u32, count: u32, axis: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> f32 {
    let n = max(vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z)) - vec3<i32>(4), vec3<i32>(0));
    let m = n + vec3<i32>(1);
    if axis > 2u || u32(m.x) * u32(m.y) * u32(m.z) > arrayLength(&buf_faces) {
        return 0.0;
    }
    let a = i32(axis);
    var dims = n;
    dims[a] = m[a];
    if idx >= u32(dims.x) * u32(dims.y) * u32(dims.z) {
        return 0.0;
    }
    let f = vec3<i32>(
        i32(idx % u32(dims.x)),
        i32((idx / u32(dims.x)) % u32(dims.y)),
        i32(idx / (u32(dims.x) * u32(dims.y))),
    );
    let s = buf_faces[u32(f.x + m.x * (f.y + m.y * f.z))];
    return select(0.0, s.face_velocity[a], s.face_weight[a] > 0.0);
}

@compute @workgroup_size(256)
fn ww_unpack_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= unpack.count { return; }
    unpack_u[idx] = ww_unpack_face(idx, unpack.count, 0u, unpack.nodes_x, unpack.nodes_y, unpack.nodes_z);
    unpack_v[idx] = ww_unpack_face(idx, unpack.count, 1u, unpack.nodes_x, unpack.nodes_y, unpack.nodes_z);
    unpack_w[idx] = ww_unpack_face(idx, unpack.count, 2u, unpack.nodes_x, unpack.nodes_y, unpack.nodes_z);
}

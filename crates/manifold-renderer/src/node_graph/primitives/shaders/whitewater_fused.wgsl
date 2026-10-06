// Whitewater stage fusion P2. Phase functions are copies of the named atom bodies.
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
@group(0) @binding(5) var<storage, read> buf_faces: array<FaceSample>;
@group(0) @binding(6) var<storage, read> buf_distance: array<f32>;
@group(0) @binding(7) var<storage, read> buf_cells: array<u32>;
@group(0) @binding(8) var<storage, read> buf_curvature: array<KnownValue>;
@group(0) @binding(9) var<storage, read> buf_turbulence: array<f32>;
@group(0) @binding(10) var<storage, read> buf_influence: array<f32>;
@group(0) @binding(11) var<storage, read_write> out_sampled: array<Element>;
@group(0) @binding(12) var<storage, read_write> out_energy: array<f32>;
@group(0) @binding(13) var<storage, read_write> out_counts: array<u32>;
@group(0) @binding(14) var<storage, read_write> out_unscaled: array<Element>;
@group(0) @binding(15) var<storage, read_write> out_wavecrest_bits: array<u32>;
// Dust has disjoint entry-point resources; its solid/source occupy the face slots.
@group(0) @binding(2) var<storage, read> buf_solid: array<f32>;
@group(0) @binding(3) var<storage, read> buf_source: array<WhitewaterSource>;
fn sf_face_len(axis: u32) -> u32 {
    if LF_PACKED {
        var dims = vec3<u32>(max(round(vec3<f32>(p.face_cells_x, p.face_cells_y, p.face_cells_z)), vec3<f32>(0.0)));
        dims[axis] += 1u;
        return dims.x * dims.y * dims.z;
    }
    if axis == 0u {
        return arrayLength(&buf_face_u);
    }
    if axis == 1u {
        return arrayLength(&buf_face_v);
    }
    return arrayLength(&buf_face_w);
}

fn sf_face(axis: u32, i: u32) -> f32 {
    if LF_PACKED {
        let face_cells = vec3<u32>(max(round(vec3<f32>(p.face_cells_x, p.face_cells_y, p.face_cells_z)), vec3<f32>(0.0)));
        var dims = face_cells;
        dims[axis] += 1u;
        let g = vec3<u32>(i % dims.x, (i / dims.x) % dims.y, i / (dims.x * dims.y));
        let m = face_cells + vec3<u32>(1u);
        let packed_index = g.x + m.x * (g.y + m.y * g.z);
        if packed_index >= arrayLength(&buf_faces) { return 0.0; }
        let s = buf_faces[packed_index];
        return select(0.0, s.face_velocity[axis], s.face_weight[axis] > 0.0);
    }
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

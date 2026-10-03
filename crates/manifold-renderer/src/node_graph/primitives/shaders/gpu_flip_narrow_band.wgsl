// Ferstl et al., Narrow Band FLIP for Liquid Simulations, CGF 35(2), 2016,
// doi:10.1111/cgf.12825, equations (3) and (4). R=3h, r=2h.
struct NbParams {
    n: vec3<u32>, slots: u32,
    minimum: vec3<f32>, h: f32,
    dt: f32, axis: u32, initialized: u32, closed_faces: u32,
}
struct NbFace { velocity: vec4<f32>, weight: vec4<f32> }
struct NbParticle { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 }
struct NbRange { start: u32, count: u32 }
@group(0) @binding(0) var<uniform> nb: NbParams;
@group(0) @binding(1) var<storage, read> nb_phi: array<f32>;
@group(0) @binding(2) var<storage, read_write> nb_out: array<f32>;
@group(0) @binding(3) var<storage, read> nb_faces: array<NbFace>;
@group(0) @binding(4) var<storage, read_write> nb_face_out: array<NbFace>;
@group(0) @binding(5) var<storage, read> nb_particle_phi: array<f32>;
@group(0) @binding(6) var<storage, read> nb_solid: array<f32>;
@group(0) @binding(7) var<storage, read_write> nb_mask: array<u32>;
@group(0) @binding(8) var<storage, read> nb_previous_phi: array<f32>;
@group(0) @binding(9) var<storage, read_write> nb_particles: array<NbParticle>;
@group(0) @binding(10) var<storage, read> nb_ranges: array<NbRange>;
@group(0) @binding(11) var<storage, read_write> nb_scan: array<u32>;
@group(0) @binding(12) var<storage, read_write> nb_status: array<u32>;
// Plan words 0 (dt) and 11 (enabled) share gpu_flip_clock's storage layout.
@group(0) @binding(46) var<storage, read> nb_clock_plan: array<u32>;
fn nb_step_dt() -> f32 {
    return select(nb.dt, bitcast<f32>(nb_clock_plan[0]), nb_clock_plan[11] != 0u);
}
fn nb_total() -> u32 { return nb.n.x * nb.n.y * nb.n.z; }
fn nb_index(p: vec3<i32>, n: vec3<i32>) -> u32 { return u32(p.x + n.x * (p.y + n.y * p.z)); }
fn nb_coords(i: u32, n: vec3<u32>) -> vec3<i32> {
    return vec3<i32>(i32(i % n.x), i32((i / n.x) % n.y), i32(i / (n.x * n.y)));
}
fn nb_scalar(q: vec3<f32>, solid: bool) -> f32 {
    let offset = select(vec3<f32>(0.5), vec3<f32>(0.0), solid);
    let size = vec3<i32>(nb.n) + select(vec3<i32>(0), vec3<i32>(1), solid);
    let s = clamp(q - offset, vec3<f32>(0.0), vec3<f32>(size - vec3<i32>(1)));
    let base = vec3<i32>(floor(s)); let t = s - vec3<f32>(base);
    var result = 0.0;
    for (var k = 0; k < 8; k++) {
        let bit = vec3<i32>(k & 1, (k >> 1) & 1, (k >> 2) & 1);
        let i = nb_index(min(base + bit, size - vec3<i32>(1)), size);
        let w = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
        var value: f32;
        if solid { value = nb_solid[i]; } else { value = nb_phi[i]; }
        result += w.x * w.y * w.z * value;
    }
    return result;
}
fn nb_velocity(q: vec3<f32>) -> vec3<f32> {
    let n = vec3<i32>(nb.n); var v = vec3<f32>(0.0);
    for (var a = 0; a < 3; a++) {
        var offset = vec3<f32>(0.5); offset[a] = 0.0;
        var top = n - vec3<i32>(1); top[a] = n[a];
        let s = clamp(q - offset, vec3<f32>(0.0), vec3<f32>(top));
        let base = vec3<i32>(floor(s)); let t = s - vec3<f32>(base);
        var weight = 0.0;
        for (var k = 0; k < 8; k++) {
            let bit = vec3<i32>(k & 1, (k >> 1) & 1, (k >> 2) & 1);
            let face = nb_faces[nb_index(min(base + bit, top), n + vec3<i32>(1))];
            let w = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
            if face.weight[a] > 0.0 {
                v[a] += w.x * w.y * w.z * face.velocity[a]; weight += w.x * w.y * w.z;
            }
        }
        if weight > 0.0 { v[a] /= weight; }
    }
    return v;
}
// RK4 backtrace, the integration order used for surface tracking in the paper.
fn nb_backtrace(q: vec3<f32>) -> vec3<f32> {
    let dt = nb_step_dt() / nb.h;
    let a = nb_velocity(q); let b = nb_velocity(q - 0.5 * dt * a);
    let c = nb_velocity(q - 0.5 * dt * b); let d = nb_velocity(q - dt * c);
    return q - (dt / 6.0) * (a + 2.0 * b + 2.0 * c + d);
}
@compute @workgroup_size(256)
fn nb_advect_phi(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= nb_total() { return; }
    let q = vec3<f32>(nb_coords(gid.x, nb.n)) + vec3<f32>(0.5);
    nb_out[gid.x] = nb_scalar(nb_backtrace(q), false);
}
@compute @workgroup_size(256)
fn nb_advect_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size = nb.n + vec3<u32>(1u);
    if gid.x >= size.x * size.y * size.z { return; }
    let p = nb_coords(gid.x, size); let n = vec3<i32>(nb.n);
    var out = NbFace(vec4<f32>(0.0), vec4<f32>(0.0));
    for (var a = 0; a < 3; a++) {
        var other = p; other[a] = 0;
        if any(other >= n) { continue; }
        var q = vec3<f32>(p) + vec3<f32>(0.5); q[a] = f32(p[a]);
        out.velocity[a] = nb_velocity(nb_backtrace(q))[a];
        out.weight[a] = select(0.0, 1.0, nb_scalar(q, false) < 0.0);
        let side = select(2u * u32(a), 2u * u32(a) + 1u, p[a] == n[a]);
        if (p[a] == 0 || p[a] == n[a]) && (nb.closed_faces & (1u << side)) != 0u {
            out.velocity[a] = 0.0; out.weight[a] = 1.0;
        }
    }
    nb_face_out[gid.x] = out;
}
@compute @workgroup_size(256)
fn nb_union(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= nb_total() { return; }
    var value = nb_particle_phi[gid.x];
    if nb.initialized != 0u { value = min(value, nb_phi[gid.x] + nb.h); }
    nb_out[gid.x] = value;
}
// Seed subcell zero crossings before the separable Manhattan transform.
@compute @workgroup_size(256)
fn nb_distance_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= nb_total() { return; }
    let n = vec3<i32>(nb.n); let p = nb_coords(gid.x, nb.n); let here = nb_phi[gid.x];
    var distance = f32(nb.n.x + nb.n.y + nb.n.z) * nb.h;
    if here == 0.0 { distance = 0.0; }
    for (var a = 0; a < 3; a++) {
        for (var d = -1; d <= 1; d += 2) {
            var q = p; q[a] += d;
            if any(q < vec3<i32>(0)) || any(q >= n) { continue; }
            let other = nb_phi[nb_index(q, n)];
            if (here < 0.0) != (other < 0.0) {
                distance = min(distance, nb.h * abs(here) / (abs(here) + abs(other)));
            }
        }
    }
    nb_out[gid.x] = select(distance, -distance, here < 0.0);
}
// One thread per axis line, forward/backward; three axes, no iteration cap.
@compute @workgroup_size(256)
fn nb_distance_sweep(@builtin(global_invocation_id) gid: vec3<u32>) {
    let a = nb.axis; let b = (a + 1u) % 3u; let c = (a + 2u) % 3u;
    if gid.x >= nb.n[b] * nb.n[c] { return; }
    var p = vec3<i32>(0); p[b] = i32(gid.x % nb.n[b]); p[c] = i32(gid.x / nb.n[b]);
    var last = f32(nb.n.x + nb.n.y + nb.n.z) * nb.h;
    for (var k = 0; k < i32(nb.n[a]); k++) {
        p[a] = k; let i = nb_index(p, vec3<i32>(nb.n)); let v = nb_out[i];
        last = min(abs(v), last + nb.h); nb_out[i] = select(last, -last, v < 0.0);
    }
    last = f32(nb.n.x + nb.n.y + nb.n.z) * nb.h;
    for (var k = i32(nb.n[a]) - 1; k >= 0; k--) {
        p[a] = k; let i = nb_index(p, vec3<i32>(nb.n)); let v = nb_out[i];
        last = min(abs(v), last + nb.h); nb_out[i] = select(last, -last, v < 0.0);
    }
}
@compute @workgroup_size(256)
fn nb_band_mask(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= nb_total() { return; }
    let q = vec3<f32>(nb_coords(gid.x, nb.n)) + vec3<f32>(0.5); let liquid = nb_phi[gid.x];
    nb_mask[gid.x] = select(0u, 1u, abs(liquid) < 3.0 * nb.h ||
        (liquid < 0.0 && nb_scalar(q, true) <= 3.0 * nb.h));
}
// Source particles may introduce a disconnected surface outside the old band.
// Include exactly the existing particle-distance gather's two-cell search
// support using cell counts; never infer liquid from the old distance alone.
@compute @workgroup_size(256)
fn nb_distance_support(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x; if i >= nb_total() { return; }
    let p = nb_coords(i, nb.n); let n = vec3<i32>(nb.n);
    let q = vec3<f32>(p) + vec3<f32>(0.5);
    var supported = abs(nb_phi[i]) < 3.0 * nb.h ||
        (nb_phi[i] < 0.0 && nb_scalar(q, true) <= 3.0 * nb.h);
    let first = max(p - vec3<i32>(2), vec3<i32>(0));
    let last = min(p + vec3<i32>(2), n - vec3<i32>(1));
    for (var z = first.z; z <= last.z && !supported; z++) {
        for (var y = first.y; y <= last.y && !supported; y++) {
            for (var x = first.x; x <= last.x && !supported; x++) {
                supported = nb_ranges[nb_index(vec3<i32>(x,y,z), n)].count != 0u;
            }
        }
    }
    nb_mask[i] = select(0u, 1u, supported);
}
// Destination holds the particle gather; source holds the advected grid.
@compute @workgroup_size(256)
fn nb_combine_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size = nb.n + vec3<u32>(1u);
    if gid.x >= size.x * size.y * size.z { return; }
    let p = nb_coords(gid.x, size); let n = vec3<i32>(nb.n); var out = nb_face_out[gid.x];
    for (var a = 0; a < 3; a++) {
        var other = p; other[a] = 0;
        if any(other >= n) { continue; }
        var q = vec3<f32>(p) + vec3<f32>(0.5); q[a] = f32(p[a]);
        let grid = nb_faces[gid.x];
        let use_particles = nb_scalar(q, false) >= -2.0 * nb.h || nb_scalar(q, true) <= 2.0 * nb.h;
        if !use_particles || out.weight[a] <= 0.0 {
            out.velocity[a] = grid.velocity[a]; out.weight[a] = grid.weight[a];
        }
    }
    nb_face_out[gid.x] = out;
}
@compute @workgroup_size(256)
fn nb_delete(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= nb.slots { return; }
    let p = nb_particles[gid.x]; if p.position_radius.w <= 0.0 { return; }
    let q = (p.position_radius.xyz - nb.minimum) / nb.h;
    if nb_scalar(q, false) < -3.0 * nb.h && nb_scalar(q, true) > 3.0 * nb.h {
        nb_particles[gid.x].position_radius.w = 0.0;
    }
}

fn nb_site(cell: vec3<i32>, site: u32) -> vec3<f32> {
    let bit = vec3<u32>(site & 1u, (site >> 1u) & 1u, (site >> 2u) & 1u);
    return vec3<f32>(cell) + vec3<f32>(0.25) + 0.5 * vec3<f32>(bit);
}
// One thread owns all eight flags of one cell. Rank within that cell is
// independent of the number of sites selected in every earlier cell.
@compute @workgroup_size(256)
fn nb_reseed_flags(@builtin(global_invocation_id) gid: vec3<u32>) {
    nb_flags(gid.x, false);
}
@compute @workgroup_size(256)
fn nb_restore_flags(@builtin(global_invocation_id) gid: vec3<u32>) {
    nb_flags(gid.x, true);
}
fn nb_flags(i: u32, restore: bool) {
    if i >= nb_total() { return; }
    for (var k = 0u; k < 8u; k++) { nb_scan[8u * i + k] = 0u; }
    if nb_phi[i] > -nb.h { return; }
    if !restore && (nb_previous_phi[i] > -3.0 * nb.h || nb_phi[i] <= -3.0 * nb.h) { return; }
    let range = nb_ranges[i]; if range.count >= 8u { return; }
    let cell = nb_coords(i, nb.n);
    var occupied = 0u;
    for (var j = 0u; j < range.count; j++) {
        let p = nb_particles[range.start + j];
        let sub = clamp(vec3<i32>(floor(2.0 * ((p.position_radius.xyz - nb.minimum) / nb.h - vec3<f32>(cell)))), vec3<i32>(0), vec3<i32>(1));
        occupied |= 1u << u32(sub.x + 2 * sub.y + 4 * sub.z);
    }
    var selected = 0u;
    for (var k = 0u; k < 8u; k++) {
        if selected >= 8u - range.count { break; }
        let q = nb_site(cell, k);
        if (occupied & (1u << k)) == 0u && nb_scalar(q, false) <= -nb.h && nb_scalar(q, true) > 0.0 {
            nb_scan[8u * i + k] = 1u; selected++;
        }
    }
}
// Validate the complete inclusive scan before any append. A shortage emits
// no partial reseed and is explicitly exposed to the liquid failure path.
@compute @workgroup_size(1)
fn nb_reseed_status() {
    let last = nb_ranges[nb_total() - 1u]; let live = last.start + last.count;
    let added = nb_scan[8u * nb_total() - 1u];
    // Branch before subtracting: a malformed live count must not wrap capacity.
    nb_status[0] = 0u;
    if live > nb.slots {
        nb_status[0] = live - nb.slots;
    } else if added > nb.slots - live {
        nb_status[0] = added - (nb.slots - live);
    }
    nb_status[1] = live + added;
}
@compute @workgroup_size(256)
fn nb_reseed_write(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= 8u * nb_total() || nb_status[0] != 0u { return; }
    var before = 0u; if i > 0u { before = nb_scan[i - 1u]; }
    if nb_scan[i] == before { return; }
    let last = nb_ranges[nb_total() - 1u]; let slot = last.start + last.count + before;
    let q = nb_site(nb_coords(i / 8u, nb.n), i % 8u);
    nb_particles[slot] = NbParticle(vec4<f32>(nb.minimum + q * nb.h, 0.31017 * nb.h), nb_velocity(q), slot + 1u);
}

// FLIP Fluids particlesheeter.cpp, interpolation.cpp, particlemaskgrid.cpp
// (MIT; see THIRD_PARTY_NOTICES.md). Sheet detection and seed selection,
// before the fill-rate draw. The contract is manifold_fluids::sheeter (the CPU
// port, proven against the native sheeter): sample points, interpolation with
// zero outside the grid, the gradient in cell units, strict signs and ties.
// The one sequential step, the mask grown seed by seed, becomes a claim: a
// sub-cell goes to the lowest engine-order candidate that passes the plane
// test, unless a marker already holds it.
struct Params {
    nx: u32, ny: u32, nz: u32, count: u32,
    bx: u32, by: u32, bz: u32, capacity: u32,
    ox: f32, oy: f32, oz: f32, h: f32,
    inv_h: f32, inv_sub: f32, half_h: f32, sub_dx: f32,
    max_depth: f32, step_distance: f32, steps: u32, max_seed_depth: f32,
    max_radius: f32, threshold: f32, pad0: u32, pad1: u32,
};
struct FluidParticle { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 };
struct CellRange { start: u32, count: u32 };
struct ClockPlan {
    step_dt: f32, elapsed: f32, remaining: f32, maximum_speed: f32,
    cap_hit: u32, nonfinite: u32, step_index: u32, event: u32,
    numerical_end: f32, marker_limit: f32, _pad0: u32, live_mode: u32,
};

@group(0) @binding(0) var<uniform> u: Params;
// Sorted by cell, stable: within a cell, in input order.
@group(0) @binding(1) var<storage, read> particles: array<FluidParticle>;
@group(0) @binding(2) var<storage, read> ranges: array<CellRange>;
// Per sorted slot: its input index, the engine's marker order.
@group(0) @binding(3) var<storage, read> order: array<u32>;
// The surface level set at cell centres, x fastest.
@group(0) @binding(4) var<storage, read> phi: array<f32>;
@group(0) @binding(5) var<storage, read_write> sheet_a: array<u32>;
@group(0) @binding(6) var<storage, read_write> sheet_b: array<u32>;
// One bit per half-cell, by ParticleMaskGrid's two floors.
@group(0) @binding(7) var<storage, read_write> mask: array<atomic<u32>>;
// Four phase-2 markers per cell: local position, w = input index bits.
@group(0) @binding(8) var<storage, read_write> selected: array<vec4<f32>>;
@group(0) @binding(9) var<storage, read_write> selected_count: array<u32>;
// Per half-cell: the lowest claiming candidate rank.
@group(0) @binding(10) var<storage, read_write> claims: array<atomic<u32>>;
// Per candidate site: projected position, w = rank, or a no-candidate mark.
@group(0) @binding(11) var<storage, read_write> sites: array<vec4<f32>>;
// Accepted births: local position, w = the source candidate's rank.
@group(0) @binding(12) var<storage, read_write> births: array<vec4<f32>>;
@group(0) @binding(13) var<storage, read_write> birth_count: array<atomic<u32>>;
@group(0) @binding(14) var<storage, read> clock_plan: array<ClockPlan>;

const NO_SITE: u32 = 0xffffffffu;
const REJECTED: u32 = 0xfffffffeu;
const NO_CLAIM: u32 = 0xffffffffu;
const MAX_PARTICLES_PER_CELL: u32 = 6u;
const MAX_SHEET_PARTICLES_PER_CELL: u32 = 4u;
const EPS: f32 = 1e-5;

fn clock_active() -> bool {
    return clock_plan[0].live_mode == 0u || clock_plan[0].step_dt > 0.0;
}

fn dims() -> vec3<i32> { return vec3<i32>(i32(u.nx), i32(u.ny), i32(u.nz)); }
fn in_range(c: vec3<i32>) -> bool { return all(c >= vec3<i32>(0)) && all(c < dims()); }
fn flat(c: vec3<i32>) -> u32 { return u32(c.x) + u.nx * (u32(c.y) + u.ny * u32(c.z)); }
fn cell_of(p: vec3<f32>) -> vec3<i32> { return vec3<i32>(floor(p * u.inv_h)); }
fn sub_of(p: vec3<f32>) -> vec3<i32> { return vec3<i32>(floor(p * u.inv_sub)); }
fn sub_case(s: vec3<i32>) -> u32 { return u32(s.x & 1) | (u32(s.y & 1) << 1u) | (u32(s.z & 1) << 2u); }
fn local(p: FluidParticle) -> vec3<f32> { return p.position_radius.xyz - vec3<f32>(u.ox, u.oy, u.oz); }

fn corner(c: vec3<i32>) -> f32 {
    if in_range(c) { return phi[flat(c)]; }
    return 0.0;
}

// Interpolation::trilinearInterpolate at p, already shifted by half a cell.
fn trilinear(p: vec3<f32>) -> f32 {
    let g = cell_of(p);
    let t = (p - vec3<f32>(g) * u.h) * u.inv_h;
    let x = t.x; let y = t.y; let z = t.z;
    return corner(g) * (1.0 - x) * (1.0 - y) * (1.0 - z)
        + corner(g + vec3<i32>(1, 0, 0)) * x * (1.0 - y) * (1.0 - z)
        + corner(g + vec3<i32>(0, 1, 0)) * (1.0 - x) * y * (1.0 - z)
        + corner(g + vec3<i32>(0, 0, 1)) * (1.0 - x) * (1.0 - y) * z
        + corner(g + vec3<i32>(1, 0, 1)) * x * (1.0 - y) * z
        + corner(g + vec3<i32>(0, 1, 1)) * (1.0 - x) * y * z
        + corner(g + vec3<i32>(1, 1, 0)) * x * y * (1.0 - z)
        + corner(g + vec3<i32>(1, 1, 1)) * x * y * z;
}

fn bilinear(v00: f32, v10: f32, v01: f32, v11: f32, ix: f32, iy: f32) -> f32 {
    let lerp1 = (1.0 - ix) * v00 + ix * v10;
    let lerp2 = (1.0 - ix) * v01 + ix * v11;
    return (1.0 - iy) * lerp1 + iy * lerp2;
}

// Interpolation::trilinearInterpolateGradient: in cell units, not over h.
fn gradient(p: vec3<f32>) -> vec3<f32> {
    let g = cell_of(p);
    let t = (p - vec3<f32>(g) * u.h) * u.inv_h;
    let v000 = corner(g);
    let v100 = corner(g + vec3<i32>(1, 0, 0));
    let v010 = corner(g + vec3<i32>(0, 1, 0));
    let v001 = corner(g + vec3<i32>(0, 0, 1));
    let v101 = corner(g + vec3<i32>(1, 0, 1));
    let v011 = corner(g + vec3<i32>(0, 1, 1));
    let v110 = corner(g + vec3<i32>(1, 1, 0));
    let v111 = corner(g + vec3<i32>(1, 1, 1));
    return vec3<f32>(
        bilinear(v100 - v000, v110 - v010, v101 - v001, v111 - v011, t.y, t.z),
        bilinear(v010 - v000, v110 - v100, v011 - v001, v111 - v101, t.x, t.z),
        bilinear(v001 - v000, v101 - v100, v011 - v010, v111 - v110, t.x, t.y));
}

fn sample(p: vec3<f32>) -> f32 { return trilinear(p - vec3<f32>(u.half_h)); }

// vmath::normalize: v times (1 / |v|).
fn unit(v: vec3<f32>) -> vec3<f32> { return v * (1.0 / length(v)); }

fn in_grid(p: vec3<f32>) -> bool {
    return all(p >= vec3<f32>(0.0)) && all(p < vec3<f32>(dims()) * u.h);
}

@compute @workgroup_size(256)
fn clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let c = gid.x;
    if c == 0u { atomicStore(&birth_count[0], 0u); }
    if c >= u.nx * u.ny * u.nz { return; }
    sheet_a[c] = 0u;
    sheet_b[c] = 0u;
    atomicStore(&mask[c], 0u);
    selected_count[c] = 0u;
    for (var o = 0u; o < 8u; o = o + 1u) {
        atomicStore(&claims[8u * c + o], NO_CLAIM);
        sites[8u * c + o] = vec4<f32>(0.0, 0.0, 0.0, bitcast<f32>(NO_SITE));
    }
}

// One thread per marker: its mask bit, and phase 1 (the depth walk).
@compute @workgroup_size(256)
fn detect(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let s = gid.x;
    if s >= u.count { return; }
    let p = local(particles[s]);
    let g = cell_of(p);
    atomicOr(&mask[flat(g)], 1u << sub_case(sub_of(p)));
    if ranges[flat(g)].count >= MAX_PARTICLES_PER_CELL { return; }
    let value = sample(p);
    if value >= u.max_depth || value < -u.max_depth { return; }
    var dir = -gradient(p - vec3<f32>(u.half_h));
    if length(dir) < EPS { return; }
    dir = unit(dir);
    var current = value;
    for (var step = 0u; step < u.steps; step = step + 1u) {
        let next = sample(p + (f32(step) * u.step_distance) * dir);
        if next > current || next >= 0.0 {
            sheet_a[flat(g)] = 1u;
            return;
        }
        current = next;
    }
}

fn grown(source: u32, c: vec3<i32>) -> u32 {
    var hit = 0u;
    for (var a = 0; a < 3; a = a + 1) {
        for (var d = -1; d <= 1; d = d + 1) {
            var q = c;
            q[a] = q[a] + d;
            if in_range(q) {
                let v = select(sheet_b[flat(q)], sheet_a[flat(q)], source == 0u);
                hit = max(hit, v);
            }
        }
    }
    return hit;
}

fn cell_coord(c: u32) -> vec3<i32> {
    return vec3<i32>(i32(c % u.nx), i32(c / u.nx % u.ny), i32(c / (u.nx * u.ny)));
}

// GridUtils::featherGrid6, first pass: a into b.
@compute @workgroup_size(256)
fn feather(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let c = gid.x;
    if c >= u.nx * u.ny * u.nz { return; }
    sheet_b[c] = grown(0u, cell_coord(c));
}

// The second feather pass, b into a, then the 3-cell border band cleared.
@compute @workgroup_size(256)
fn feather_border(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let c = gid.x;
    if c >= u.nx * u.ny * u.nz { return; }
    let q = cell_coord(c);
    let border = any(q < vec3<i32>(3)) || any(q >= dims() - vec3<i32>(3));
    sheet_a[c] = select(grown(1u, q), 0u, border);
}

// Phase 2: the first four markers of each sheet cell, in input order, with
// -2h <= phi < 2h.
@compute @workgroup_size(256)
fn select_markers(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let c = gid.x;
    if c >= u.nx * u.ny * u.nz || sheet_a[c] == 0u { return; }
    let range = ranges[c];
    var n = 0u;
    for (var s = range.start; s < range.start + range.count && n < MAX_SHEET_PARTICLES_PER_CELL; s = s + 1u) {
        let p = local(particles[s]);
        let value = sample(p);
        if value >= u.max_depth || value < -u.max_depth { continue; }
        selected[4u * c + n] = vec4<f32>(p, bitcast<f32>(order[s]));
        n = n + 1u;
    }
    selected_count[c] = n;
}

// One coarse 2-cell bucket's phase-2 markers in input order: the eight
// cells' lists (each in input order) merged.
var<private> bucket: array<vec4<f32>, 32>;
fn fill_bucket(b: vec3<i32>) -> u32 {
    var heads: array<u32, 8>;
    var counts: array<u32, 8>;
    var cells: array<u32, 8>;
    for (var l = 0; l < 8; l = l + 1) {
        let c = 2 * b + vec3<i32>(l & 1, (l >> 1) & 1, (l >> 2) & 1);
        heads[l] = 0u;
        counts[l] = 0u;
        if in_range(c) {
            cells[l] = flat(c);
            counts[l] = selected_count[cells[l]];
        }
    }
    var n = 0u;
    loop {
        var best = -1;
        var best_index = 0xffffffffu;
        for (var l = 0; l < 8; l = l + 1) {
            if heads[l] < counts[l] {
                let index = bitcast<u32>(selected[4u * cells[l] + heads[l]].w);
                if index < best_index { best_index = index; best = l; }
            }
        }
        if best < 0 { break; }
        bucket[n] = selected[4u * cells[best] + heads[best]];
        heads[best] = heads[best] + 1u;
        n = n + 1u;
    }
    return n;
}

fn bucket_dims() -> vec3<i32> { return vec3<i32>(i32(u.bx), i32(u.by), i32(u.bz)); }

// One thread per half-cell site: candidate test, plane projection, mask and
// opposite-neighbour test; a passing candidate claims its sub-cell.
@compute @workgroup_size(256)
fn candidates(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let site = gid.x;
    if site >= 8u * u.nx * u.ny * u.nz { return; }
    let c = site / 8u;
    if sheet_a[c] == 0u { return; }
    let o = site % 8u;
    let q = cell_coord(c);
    // Offsets in the engine's order, k fastest.
    let d = vec3<i32>(i32((o >> 2u) & 1u), i32((o >> 1u) & 1u), i32(o & 1u));
    let s = 2 * q + d;
    let seed = vec3<f32>(s) * u.sub_dx + vec3<f32>(0.5 * u.sub_dx);
    let value = sample(seed);
    if value >= 0.0 || value < -u.max_seed_depth { return; }
    // The engine visits candidates by bucket (k, j, i), then cell (k, j, i)
    // within it, then offset: this rank is that visiting order.
    let b = q / 2;
    let in_bucket = u32((q.z & 1) * 4 + (q.y & 1) * 2 + (q.x & 1));
    let bucket_index = u32(b.x) + u.bx * (u32(b.y) + u.by * u32(b.z));
    let rank = (bucket_index * 8u + in_bucket) * 8u + o;
    sites[site] = vec4<f32>(seed, bitcast<f32>(REJECTED));

    var centroid = vec3<f32>(0.0);
    var near = 0u;
    var len1 = 1e6; var len2 = 1e6; var len3 = 1e6;
    var p1 = vec3<f32>(0.0); var p2 = vec3<f32>(0.0); var p3 = vec3<f32>(0.0);
    for (var k = b.z - 1; k <= b.z + 1; k = k + 1) {
        for (var j = b.y - 1; j <= b.y + 1; j = j + 1) {
            for (var i = b.x - 1; i <= b.x + 1; i = i + 1) {
                let nb = vec3<i32>(i, j, k);
                if any(nb < vec3<i32>(0)) || any(nb >= bucket_dims()) { continue; }
                let n = fill_bucket(nb);
                for (var e = 0u; e < n; e = e + 1u) {
                    let np = bucket[e].xyz;
                    let len = length(np - seed);
                    if !(len < u.max_radius) { continue; }
                    centroid = centroid + np;
                    near = near + 1u;
                    if len < len1 {
                        len3 = len2; len2 = len1; len1 = len;
                        p3 = p2; p2 = p1; p1 = np;
                    } else if len < len2 {
                        len3 = len2; len2 = len;
                        p3 = p2; p2 = np;
                    } else if len < len3 {
                        len3 = len;
                        p3 = np;
                    }
                }
            }
        }
    }
    if near < 3u { return; }
    centroid = centroid * (1.0 / f32(near));
    let vt1 = p2 - p1;
    let vt2 = p3 - p1;
    let cr = cross(vt1, vt2);
    if length(vt1) < EPS || length(vt2) < EPS || length(cr) < EPS { return; }
    let normal = unit(cr);
    let distance = -dot(normal, seed - p1);
    let p = seed + (0.75 * distance) * normal;
    if !in_grid(p) { return; }
    let ps = sub_of(p);
    let pc = flat(ps / 2);
    let bit = sub_case(ps);
    if (atomicLoad(&mask[pc]) & (1u << bit)) != 0u { return; }
    var cdir = centroid - p;
    if length(cdir) < EPS { return; }
    cdir = unit(cdir);
    var mindot = 1.01;
    for (var k = b.z - 1; k <= b.z + 1; k = k + 1) {
        for (var j = b.y - 1; j <= b.y + 1; j = j + 1) {
            for (var i = b.x - 1; i <= b.x + 1; i = i + 1) {
                let nb = vec3<i32>(i, j, k);
                if any(nb < vec3<i32>(0)) || any(nb >= bucket_dims()) { continue; }
                let n = fill_bucket(nb);
                for (var e = 0u; e < n; e = e + 1u) {
                    let np = bucket[e].xyz;
                    if !(length(np - seed) < u.max_radius) { continue; }
                    let ndir = np - p;
                    if length(ndir) < EPS { continue; }
                    mindot = min(mindot, dot(cdir, unit(ndir)));
                }
            }
        }
    }
    if !(mindot < u.threshold) { return; }
    sites[site] = vec4<f32>(p, bitcast<f32>(rank));
    atomicMin(&claims[8u * pc + bit], rank);
}

// A claimant that holds its sub-cell is born, up to the list's capacity; the
// count is the true total.
@compute @workgroup_size(256)
fn resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let site = gid.x;
    if site >= 8u * u.nx * u.ny * u.nz { return; }
    let v = sites[site];
    let rank = bitcast<u32>(v.w);
    if rank >= REJECTED { return; }
    let ps = sub_of(v.xyz);
    if atomicLoad(&claims[8u * flat(ps / 2) + sub_case(ps)]) != rank { return; }
    let slot = atomicAdd(&birth_count[0], 1u);
    if slot < u.capacity {
        births[slot] = vec4<f32>(v.xyz, v.w);
    }
}

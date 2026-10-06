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
    max_radius: f32, threshold: f32, inv_h_bits_lo: u32, inv_h_bits_hi: u32,
    inv_sub_bits_lo: u32, inv_sub_bits_hi: u32, rate: f32, tick: u32,
    substep: u32, slots: u32, pad0: u32, pad1: u32,
};
struct FluidParticle { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 };
struct CellRange { start: u32, count: u32 };
struct FaceSample { face_velocity: vec4<f32>, face_weight: vec4<f32> };
struct ClockPlan {
    step_dt: f32, elapsed: f32, remaining: f32, maximum_speed: f32,
    cap_hit: u32, nonfinite: u32, step_index: u32, event: u32,
    numerical_end: f32, marker_limit: f32, _pad0: u32, live_mode: u32,
};

@group(0) @binding(0) var<uniform> u: Params;
// Sorted by cell, stable: within a cell, in input order.
@group(0) @binding(1) var<storage, read_write> particles: array<FluidParticle>;
@group(0) @binding(2) var<storage, read> ranges: array<CellRange>;
// Per sorted slot: its input index, the engine's marker order.
@group(0) @binding(3) var<storage, read> order: array<u32>;
// The surface level set at cell centres, x fastest.
@group(0) @binding(4) var<storage, read> phi: array<f32>;
@group(0) @binding(5) var<storage, read_write> sheet_a: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read_write> sheet_b: array<u32>;
// One bit per half-cell, by ParticleMaskGrid's two floors.
@group(0) @binding(7) var<storage, read_write> mask: array<atomic<u32>>;
// Fixed 32-slot bucket rows. Selection writes eight four-marker segments;
// build_buckets merges each row before evaluation. w stays input index bits.
@group(0) @binding(8) var<storage, read_write> selected: array<vec4<f32>>;
// Hard cap: at most four selected markers per cell.
@group(0) @binding(9) var<storage, read_write> cell_counts: array<u32>;
// Per half-cell: the lowest claiming candidate rank.
@group(0) @binding(10) var<storage, read_write> claims: array<atomic<u32>>;
// Per cell: bit o marks offset o a candidate, bit 8 + o a claimant.
@group(0) @binding(11) var<storage, read_write> flags: array<atomic<u32>>;
// Accepted births in rank order (the engine's): local position, w = rank.
@group(0) @binding(12) var<storage, read_write> births: array<vec4<f32>>;
@group(0) @binding(13) var<storage, read_write> birth_count: array<atomic<u32>>;
@group(0) @binding(14) var<storage, read> clock_plan: array<ClockPlan>;
// Per rank: 1 for a claimant holding its sub-cell, scanned in place (inclusive).
@group(0) @binding(15) var<storage, read_write> winners: array<u32>;
// In the step only. The constrained saved faces (gpu_flip_step.wgsl layout).
@group(0) @binding(16) var<storage, read> old: array<FaceSample>;
// liquid_state's birth identity: next, epoch, reserved base, reset request.
@group(0) @binding(17) var<storage, read> birth_identity: array<u32>;
// The last active substep's requested and written births, then both summed.
@group(0) @binding(18) var<storage, read_write> stats: array<u32>;

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
// Grid3d::positionToGridIndex floors the f64 product of the widened position
// and the f64 reciprocal. Emulated exactly in integers: the 24-bit mantissa
// times the 53-bit one, rounded to 53 bits nearest-even, then floored.
fn mul32(a: u32, b: u32) -> vec2<u32> {
    let a0 = a & 0xffffu; let a1 = a >> 16u;
    let b0 = b & 0xffffu; let b1 = b >> 16u;
    let p00 = a0 * b0; let p01 = a0 * b1; let p10 = a1 * b0; let p11 = a1 * b1;
    let mid = (p00 >> 16u) + (p01 & 0xffffu) + (p10 & 0xffffu);
    return vec2<u32>((p00 & 0xffffu) | (mid << 16u), p11 + (p01 >> 16u) + (p10 >> 16u) + (mid >> 16u));
}
fn limb_bit(x: vec3<u32>, i: u32) -> u32 { return (x[i / 32u] >> (i % 32u)) & 1u; }
// x >> s for s < 96, the low two limbs.
fn shr96(x: vec3<u32>, s: u32) -> vec2<u32> {
    var out = vec2<u32>(0u);
    for (var k = 0u; k < 2u; k = k + 1u) {
        let at = 32u * k + s;
        let w = at / 32u; let b = at % 32u;
        var v = 0u;
        if w < 3u { v = x[w] >> b; }
        if b != 0u && w + 1u < 3u { v = v | (x[w + 1u] << (32u - b)); }
        out[k] = v;
    }
    return out;
}
// Any bit of x below bit i.
fn below(x: vec3<u32>, i: u32) -> bool {
    for (var w = 0u; w < 3u; w = w + 1u) {
        if 32u * w >= i { break; }
        let n = min(32u, i - 32u * w);
        let mask = select((1u << n) - 1u, 0xffffffffu, n == 32u);
        if (x[w] & mask) != 0u { return true; }
    }
    return false;
}
// The engine's floor for results in (-2^30, 2^30). Past that, and for
// non-finite input, a defined GPU policy, not native parity (native casts
// the floor to int, undefined for NaN and out-of-range values): clamp to
// +-2^30, NaN to +2^30, outside every lattice. Indices from it are range
// checked (in_range, the zero-outside corners) or come from positions that
// already passed the grid check (the sub-cell mask and claim accesses).
const INDEX_LIMIT: i32 = 1073741824;
fn exact_floor(p: f32, r: vec2<u32>) -> i32 {
    if p == 0.0 { return 0; }
    // Exponent bits, not a comparison: fast math may fold a NaN test away.
    let raw = bitcast<u32>(p);
    if (raw & 0x7f800000u) == 0x7f800000u {
        return select(INDEX_LIMIT, -INDEX_LIMIT, raw == 0xff800000u);
    }
    // The product's magnitude rounds the same either sign (nearest-even), so
    // the magnitude is floored and a negative fractional result steps down.
    let negative = p < 0.0;
    let bits = bitcast<u32>(abs(p));
    let e = (bits >> 23u) & 255u;
    let m = (bits & 0x7fffffu) | select(0u, 0x800000u, e != 0u);
    let ep = select(i32(e) - 150, -149, e == 0u);
    let er = i32((r.y >> 20u) & 2047u) - 1075;
    let lo = mul32(m, r.x);
    let hi = mul32(m, (r.y & 0xfffffu) | 0x100000u);
    let l1 = lo.y + hi.x;
    let x = vec3<u32>(lo.x, l1, hi.y + select(0u, 1u, l1 < lo.y));
    var length = 0u;
    if x.z != 0u { length = 96u - countLeadingZeros(x.z); }
    else if x.y != 0u { length = 64u - countLeadingZeros(x.y); }
    else { length = 32u - countLeadingZeros(x.x); }
    var q = vec2<u32>(x.x, x.y);
    var exponent = ep + er;
    if length > 53u {
        let d = length - 53u;
        q = shr96(x, d);
        let round = limb_bit(x, d - 1u) == 1u;
        if round && (below(x, d - 1u) || (q.x & 1u) == 1u) {
            q.x = q.x + 1u;
            if q.x == 0u { q.y = q.y + 1u; }
        }
        exponent = exponent + i32(d);
    }
    if exponent >= 0 { return select(INDEX_LIMIT, -INDEX_LIMIT, negative); }
    let s = u32(-exponent);
    var whole = 0u;
    var fraction = true;
    if s < 64u {
        let shifted = shr96(vec3<u32>(q, 0u), s);
        if shifted.y != 0u || shifted.x >= u32(INDEX_LIMIT) { return select(INDEX_LIMIT, -INDEX_LIMIT, negative); }
        whole = shifted.x;
        fraction = below(vec3<u32>(q, 0u), s);
    }
    if !negative { return i32(whole); }
    return -i32(whole) - select(0, 1, fraction);
}
fn exact_cell(p: vec3<f32>, r: vec2<u32>) -> vec3<i32> {
    return vec3<i32>(exact_floor(p.x, r), exact_floor(p.y, r), exact_floor(p.z, r));
}
fn cell_of(p: vec3<f32>) -> vec3<i32> { return exact_cell(p, vec2<u32>(u.inv_h_bits_lo, u.inv_h_bits_hi)); }
fn sub_of(p: vec3<f32>) -> vec3<i32> { return exact_cell(p, vec2<u32>(u.inv_sub_bits_lo, u.inv_sub_bits_hi)); }
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
    if c == 0u { atomicStore(&birth_count[1], 0u); }
    if c >= u.nx * u.ny * u.nz { return; }
    atomicStore(&sheet_a[c], 0u);
    sheet_b[c] = 0u;
    atomicStore(&mask[c], 0u);
    cell_counts[c] = 0u;
    atomicStore(&flags[c], 0u);
    for (var o = 0u; o < 8u; o = o + 1u) {
        atomicStore(&claims[8u * c + o], NO_CLAIM);
    }
}

// One thread per marker: its mask bit, and phase 1 (the depth walk).
@compute @workgroup_size(256)
fn detect(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let s = gid.x;
    if s >= u.count { return; }
    // In the step, count is every slot: the sort leaves a dead tail at radius 0.
    if !(particles[s].position_radius.w > 0.0) { return; }
    let p = local(particles[s]);
    let g = cell_of(p);
    if !in_range(g) { return; }
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
            atomicStore(&sheet_a[flat(g)], 1u);
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
                var v = sheet_b[flat(q)];
                if source == 0u { v = atomicLoad(&sheet_a[flat(q)]); }
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
    atomicStore(&sheet_a[c], select(grown(1u, q), 0u, border));
}

// Phase 2: the first four markers of each sheet cell, in input order, with
// -2h <= phi < 2h.
@compute @workgroup_size(256)
fn select_markers(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let c = gid.x;
    if c >= u.nx * u.ny * u.nz || atomicLoad(&sheet_a[c]) == 0u { return; }
    let range = ranges[c];
    var n = 0u;
    for (var s = range.start; s < range.start + range.count && n < MAX_SHEET_PARTICLES_PER_CELL; s = s + 1u) {
        let p = local(particles[s]);
        let value = sample(p);
        if value >= u.max_depth || value < -u.max_depth { continue; }
        selected[segment_base(cell_coord(c)) + n] = vec4<f32>(p, bitcast<f32>(order[s]));
        n = n + 1u;
    }
    cell_counts[c] = n;
}

fn bucket_flat(b: vec3<i32>) -> u32 {
    return u32(b.x) + u.bx * (u32(b.y) + u.by * u32(b.z));
}

fn segment_base(c: vec3<i32>) -> u32 {
    let lane = u32(c.x & 1) + 2u * u32(c.y & 1) + 4u * u32(c.z & 1);
    return 32u * bucket_flat(c / 2) + 4u * lane;
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
            counts[l] = cell_counts[cells[l]];
        }
    }
    var n = 0u;
    loop {
        var best = -1;
        var best_index = 0xffffffffu;
        for (var l = 0; l < 8; l = l + 1) {
            if heads[l] < counts[l] {
                let index = bitcast<u32>(selected[32u * bucket_flat(b) + 4u * u32(l) + heads[l]].w);
                if index < best_index { best_index = index; best = l; }
            }
        }
        if best < 0 { break; }
        bucket[n] = selected[32u * bucket_flat(b) + 4u * u32(best) + heads[best]];
        heads[best] = heads[best] + 1u;
        n = n + 1u;
    }
    return n;
}

// One invocation owns a whole row. Complete the merge in private memory
// before overwriting any source segment (including unread heads).
@compute @workgroup_size(256)
fn build_buckets(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let row = gid.x;
    if row >= u.bx * u.by * u.bz { return; }
    let b = vec3<i32>(i32(row % u.bx), i32(row / u.bx % u.by), i32(row / (u.bx * u.by)));
    let n = fill_bucket(b);
    for (var m = 0u; m < n; m = m + 1u) {
        selected[32u * row + m] = bucket[m];
    }
    // Feathering is finished: sheet_b is now the bucket row lengths.
    sheet_b[row] = n;
}

fn bucket_dims() -> vec3<i32> { return vec3<i32>(i32(u.bx), i32(u.by), i32(u.bz)); }

struct Evaluation {
    // 0: not a candidate, 1: a candidate that fails a test, 2: a claimant.
    state: u32,
    p: vec3<f32>,
    rank: u32,
};

// One half-cell site: candidate test, plane projection, mask and
// opposite-neighbour test. Deterministic, so resolve recomputes a claimant's
// projection instead of storing it.
fn evaluate(site: u32) -> Evaluation {
    var e = Evaluation(0u, vec3<f32>(0.0), 0u);
    let c = site / 8u;
    if atomicLoad(&sheet_a[c]) == 0u { return e; }
    let o = site % 8u;
    let q = cell_coord(c);
    // Offsets in the engine's order, k fastest.
    let d = vec3<i32>(i32((o >> 2u) & 1u), i32((o >> 1u) & 1u), i32(o & 1u));
    let s = 2 * q + d;
    // (f32)s · sub_dx + sub_dx/2 in f64 is exact, so one f32 rounding matches.
    let seed = fma(vec3<f32>(s), vec3<f32>(u.sub_dx), vec3<f32>(0.5 * u.sub_dx));
    let value = sample(seed);
    if value >= 0.0 || value < -u.max_seed_depth { return e; }
    e.state = 1u;
    // The engine visits candidates by bucket (k, j, i), then cell (k, j, i)
    // within it, then offset: this rank is that visiting order.
    let b = q / 2;
    let in_bucket = u32((q.z & 1) * 4 + (q.y & 1) * 2 + (q.x & 1));
    let bucket_index = u32(b.x) + u.bx * (u32(b.y) + u.by * u32(b.z));
    e.rank = (bucket_index * 8u + in_bucket) * 8u + o;

    var centroid = vec3<f32>(0.0);
    var near = 0u;
    var len1 = 1e6; var len2 = 1e6; var len3 = 1e6;
    var p1 = vec3<f32>(0.0); var p2 = vec3<f32>(0.0); var p3 = vec3<f32>(0.0);
    for (var k = b.z - 1; k <= b.z + 1; k = k + 1) {
        for (var j = b.y - 1; j <= b.y + 1; j = j + 1) {
            for (var i = b.x - 1; i <= b.x + 1; i = i + 1) {
                let nb = vec3<i32>(i, j, k);
                if any(nb < vec3<i32>(0)) || any(nb >= bucket_dims()) { continue; }
                let row = bucket_flat(nb);
                let n = sheet_b[row];
                for (var m = 0u; m < n; m = m + 1u) {
                    let np = selected[32u * row + m].xyz;
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
    if near < 3u { return e; }
    centroid = centroid * (1.0 / f32(near));
    let vt1 = p2 - p1;
    let vt2 = p3 - p1;
    let cr = cross(vt1, vt2);
    if length(vt1) < EPS || length(vt2) < EPS || length(cr) < EPS { return e; }
    let normal = unit(cr);
    let distance = -dot(normal, seed - p1);
    let p = seed + (0.75 * distance) * normal;
    if !in_grid(p) { return e; }
    let ps = sub_of(p);
    if (atomicLoad(&mask[flat(ps / 2)]) & (1u << sub_case(ps))) != 0u { return e; }
    var cdir = centroid - p;
    if length(cdir) < EPS { return e; }
    cdir = unit(cdir);
    var mindot = 1.01;
    for (var k = b.z - 1; k <= b.z + 1; k = k + 1) {
        for (var j = b.y - 1; j <= b.y + 1; j = j + 1) {
            for (var i = b.x - 1; i <= b.x + 1; i = i + 1) {
                let nb = vec3<i32>(i, j, k);
                if any(nb < vec3<i32>(0)) || any(nb >= bucket_dims()) { continue; }
                let row = bucket_flat(nb);
                let n = sheet_b[row];
                for (var m = 0u; m < n; m = m + 1u) {
                    let np = selected[32u * row + m].xyz;
                    if !(length(np - seed) < u.max_radius) { continue; }
                    let ndir = np - p;
                    if length(ndir) < EPS { continue; }
                    mindot = min(mindot, dot(cdir, unit(ndir)));
                }
            }
        }
    }
    if !(mindot < u.threshold) { return e; }
    e.state = 2u;
    e.p = p;
    return e;
}

// One thread per half-cell site: flag candidates and claimants; a claimant
// claims its sub-cell for its rank.
@compute @workgroup_size(256)
fn candidates(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let site = gid.x;
    if site >= 8u * u.nx * u.ny * u.nz { return; }
    let e = evaluate(site);
    if e.state == 0u { return; }
    let c = site / 8u;
    let o = site % 8u;
    atomicOr(&flags[c], 1u << o);
    if e.state == 2u {
        atomicOr(&flags[c], 1u << (8u + o));
        let ps = sub_of(e.p);
        atomicMin(&claims[8u * flat(ps / 2) + sub_case(ps)], e.rank);
    }
}

fn rank_total() -> u32 { return 64u * u.bx * u.by * u.bz; }

// The half-cell site a rank names, or NO_CLAIM where its cell is past the
// grid (an odd side's last bucket).
fn site_of_rank(r: u32) -> u32 {
    let o = r % 8u;
    let in_bucket = (r / 8u) % 8u;
    let bucket = r / 64u;
    let b = vec3<i32>(i32(bucket % u.bx), i32(bucket / u.bx % u.by), i32(bucket / (u.bx * u.by)));
    let q = 2 * b + vec3<i32>(i32(in_bucket & 1u), i32((in_bucket >> 1u) & 1u), i32((in_bucket >> 2u) & 1u));
    if !in_range(q) { return NO_CLAIM; }
    return 8u * flat(q) + o;
}

// A claimant that holds its sub-cell, recomputed: state 2 and its claim;
// state 3 a claimant that passes but lost its sub-cell to a lower rank.
fn holds(site: u32) -> Evaluation {
    var e = Evaluation(0u, vec3<f32>(0.0), 0u);
    if site == NO_CLAIM { return e; }
    if (atomicLoad(&flags[site / 8u]) & (1u << (8u + site % 8u))) == 0u { return e; }
    e = evaluate(site);
    if e.state != 2u { return e; }
    let ps = sub_of(e.p);
    if atomicLoad(&claims[8u * flat(ps / 2) + sub_case(ps)]) != e.rank { e.state = 3u; }
    return e;
}

// One thread per rank: 1 where that candidate holds its sub-cell. The scan
// over ranks then numbers the births in the engine's order.
@compute @workgroup_size(256)
fn resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let r = gid.x;
    if r >= rank_total() { return; }
    // The engine keeps a seed unless its draw is over the rate; standalone
    // runs at rate 1, which keeps every one (the draw is under 1).
    let site = site_of_rank(r);
    let e = holds(site);
    // A flagged claimant that no longer passes on recomputation would lose
    // its claim silently: count it with the failures (always 0).
    if site != NO_CLAIM && (atomicLoad(&flags[site / 8u]) & (1u << (8u + site % 8u))) != 0u && e.state < 2u {
        atomicAdd(&birth_count[1], 1u);
    }
    winners[r] = select(0u, 1u, e.state == 2u && u.rate > 0.0 && fill_draw(r) <= u.rate);
}

// After the scan: each birth at its index in engine order, up to the list's
// capacity; word 0 of the count is the true total. Word 1 counts claimants
// whose recomputed evaluation disagrees with the claim pass, here or in
// resolve (a proof that must stay 0).
@compute @workgroup_size(256)
fn place(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let r = gid.x;
    let total = rank_total();
    if r == 0u { atomicStore(&birth_count[0], winners[total - 1u]); }
    if r >= total { return; }
    var before = 0u;
    if r > 0u { before = winners[r - 1u]; }
    if winners[r] == before { return; }
    let e = holds(site_of_rank(r));
    if e.state != 2u || e.rank != r { atomicAdd(&birth_count[1], 1u); }
    if before < u.capacity {
        births[before] = vec4<f32>(e.p, bitcast<f32>(r));
    }
}

// Proof only (GpuSheeting::encode_index_probe): each particle's cell and
// half-cell as the stage decides them, two records a particle.
@compute @workgroup_size(256)
fn index_probe(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if s >= u.count { return; }
    let p = local(particles[s]);
    births[2u * s] = bitcast<vec4<f32>>(vec4<i32>(cell_of(p), 0));
    births[2u * s + 1u] = bitcast<vec4<f32>>(vec4<i32>(sub_of(p), 0));
}

// ---- In the step: the fill-rate draw (in resolve), then after the rank
// scan and the caller's identity reservation, the tail write. ----

// PCG hash (Jarzynski & Olano 2020).
fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

// The fill-rate draw, uniform in [0, 1) at 24 bits. A pure function of the
// candidate's rank, the tick and the substep: no state to seed or reset, so
// a replayed or re-run tick draws the same births, and a reset (tick 0
// again) repeats the first run's. A 24-bit draw can be 0, so rate 0 is
// refused in resolve as well as never encoded.
fn fill_draw(rank: u32) -> f32 {
    let h = pcg(rank ^ pcg(u.tick ^ pcg(u.substep ^ 0x5eedf111u)));
    return f32(h >> 8u) * (1.0 / 16777216.0);
}

fn finite3(v: vec3<f32>) -> bool {
    let bits = bitcast<vec3<u32>>(v) & vec3<u32>(0x7f800000u);
    return all(bits != vec3<u32>(0x7f800000u));
}

// gpu_flip_step.wgsl `sample` on the saved faces: q in cells from the grid's
// minimum, missing corners contributing zero.
fn saved_velocity(q: vec3<f32>) -> vec3<f32> {
    let n = dims();
    if !finite3(q) || any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(n)) {
        return vec3<f32>(0.0);
    }
    let m = n + vec3<i32>(1);
    var v = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        var offset = vec3<f32>(0.5);
        offset[a] = 0.0;
        var top = n - vec3<i32>(1);
        top[a] = n[a];
        let s = q - offset;
        let base = vec3<i32>(floor(s));
        let t = s - vec3<f32>(base);
        var sum = 0.0;
        for (var corner = 0; corner < 8; corner = corner + 1) {
            let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
            let c = base + bit;
            if all(c >= vec3<i32>(0)) && all(c <= top) {
                let face = old[u32(c.x) + u32(m.x) * (u32(c.y) + u32(m.y) * u32(c.z))];
                let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
                sum = sum + w3.x * w3.y * w3.z * face.face_velocity[a];
            }
        }
        v[a] = sum;
    }
    return v;
}

// Each drawn birth into the slot after the live prefix its scan index gives,
// up to the pool's slots, in the saved velocity at its seed (the engine's
// birth velocity; the FLIP update then gives it the new field's), with the
// identity the reservation set aside. Births past the pool are dropped and
// counted. The birth list keeps each seed at its birth index, for the proofs.
@compute @workgroup_size(256)
fn write(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    if birth_identity[3] != 0u { return; }
    let r = gid.x;
    let total = rank_total();
    let last = ranges[u.nx * u.ny * u.nz - 1u];
    let live = last.start + last.count;
    let room = select(0u, u.slots - live, live <= u.slots);
    if r == 0u {
        let requested = winners[total - 1u];
        atomicStore(&birth_count[0], requested);
        stats[0] = requested;
        stats[1] = min(requested, room);
        stats[2] = stats[2] + requested;
        stats[3] = stats[3] + min(requested, room);
    }
    if r >= total { return; }
    var before = 0u;
    if r > 0u { before = winners[r - 1u]; }
    if winners[r] == before { return; }
    let e = holds(site_of_rank(r));
    if e.state != 2u || e.rank != r { atomicAdd(&birth_count[1], 1u); }
    if before < u.capacity {
        births[before] = vec4<f32>(e.p, bitcast<f32>(r));
    }
    if before >= room { return; }
    let origin = vec3<f32>(u.ox, u.oy, u.oz);
    let world = e.p + origin;
    // (3 / (4π · 8))^(1/3): the sphere of an eighth of a cell, as the fill's.
    particles[live + before] = FluidParticle(vec4<f32>(world, 0.31017524 * u.h),
        saved_velocity((world - origin) / u.h), birth_identity[2] + before);
}

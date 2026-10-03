// Lentine 2010 §§3.2–3.4, component generalisation in the design §11.1.
// Integrated flux units, h=1. P is the block-component indicator and the
// boundary operator is P^T A P / 2. Parallel subfaces remain distinct.
struct Params { n: vec3<u32>, count: u32, tolerance: f32, pad0: u32, pad1: u32, pad2: u32 }
struct Progress {
    status: u32, iterations: u32, failed: u32, pad: u32,
    rr: f32, alpha: f32, beta: f32, initial: f32,
    residual: f32,
}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> water: array<f32>;
@group(0) @binding(2) var<storage, read> links: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> source: array<f32>;
@group(0) @binding(4) var<storage, read_write> labels: array<u32>;
@group(0) @binding(5) var<storage, read_write> rhs: array<f32>;
@group(0) @binding(6) var<storage, read_write> pressure: array<f32>;
@group(0) @binding(7) var<storage, read_write> pockets: array<u32>;
@group(0) @binding(8) var<storage, read_write> sums: array<vec4<f32>>;
@group(0) @binding(9) var<storage, read_write> compensation: array<vec4<f32>>;
// r, direction, A*direction, reserved
@group(0) @binding(10) var<storage, read_write> vectors: array<vec4<f32>>;
@group(0) @binding(11) var<storage, read_write> progress: Progress;
@group(0) @binding(12) var<storage, read_write> transfer: array<vec4<f32>>;
const NONE: u32 = 0xffffffffu;
const EPSILON: f32 = 1.1920928955078125e-7;
fn xyz(i: u32, n: vec3<u32>) -> vec3<u32> { return vec3(i % n.x, (i / n.x) % n.y, i / (n.x * n.y)); }
fn index(p: vec3<u32>) -> u32 { return p.x + params.n.x * (p.y + params.n.y * p.z); }
fn child(p: vec3<u32>, k: u32) -> u32 {
    let q = p + vec3(k & 1u, (k >> 1u) & 1u, k >> 2u);
    if any(q >= params.n) { return NONE; }
    return index(q);
}
fn neighbor(i: u32, a: u32, high: bool) -> u32 {
    var p = xyz(i, params.n);
    if high { if p[a] + 1u >= params.n[a] { return NONE; } p[a] += 1u; }
    else { if p[a] == 0u { return NONE; } p[a] -= 1u; }
    return index(p);
}
fn wet(i: u32) -> bool { if i == NONE { return false; } return water[i] > 0.5; }
fn is_root(i: u32) -> bool { return labels[i] == i; }
fn finite(v: f32) -> bool { return abs(v) <= 3.402823466e38; }

// Eight vertices are the mathematical block, not a resolution/quality cap.
@compute @workgroup_size(256)
fn labels_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = (params.n + vec3(1u)) / 2u;
    if gid.x >= n.x * n.y * n.z { return; }
    let base = xyz(gid.x, n) * 2u;
    var ids: array<u32, 8>;
    var roots: array<u32, 8>;
    for (var k = 0u; k < 8u; k++) {
        ids[k] = child(base, k); roots[k] = NONE;
        if wet(ids[k]) { roots[k] = ids[k]; }
    }
    // At most seven edges on any simple path through eight vertices.
    for (var round = 0u; round < 7u; round++) {
        for (var k = 0u; k < 8u; k++) {
            if roots[k] == NONE { continue; }
            for (var a = 0u; a < 3u; a++) {
                let j = k ^ (1u << a);
                if roots[j] == NONE { continue; }
                let low = select(j, k, (k & (1u << a)) == 0u);
                if links[ids[low]][a] > 0.0 {
                    let r = min(roots[k], roots[j]); roots[k] = r; roots[j] = r;
                }
            }
        }
    }
    for (var k = 0u; k < 8u; k++) { if ids[k] != NONE { labels[ids[k]] = roots[k]; } }
}

@compute @workgroup_size(256)
fn gather_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x; if i >= params.count { return; }
    rhs[i] = 0.0; pressure[i] = 0.0; pockets[i] = i;
    sums[i] = vec4(0.0); compensation[i] = vec4(0.0); vectors[i] = vec4(0.0);
    if !is_root(i) { return; }
    let base = (xyz(i, params.n) / 2u) * 2u;
    var b = 0.0;
    for (var k = 0u; k < 8u; k++) {
        let j = child(base, k); if j == NONE { continue; }
        if labels[j] == i { b += source[j]; }
    }
    rhs[i] = b;
}

fn root(start: u32) -> u32 {
    var r = start;
    loop { if pockets[r] == r { break; } r = pockets[r]; }
    return r;
}

// Deterministic union/find and compensated pocket sums, O(cells + edges)
// storage. No readback, atomics, fixed pocket count, or cross-pocket averaging.
// Serial topology setup is intentionally separate from parallel row products;
// performance remains unmeasured at this staging seam.
@compute @workgroup_size(1)
fn pockets_main() {
    progress.status = 0u; progress.iterations = 0u; progress.failed = NONE; progress.pad = 0u;
    progress.rr = 0.0; progress.alpha = 0.0; progress.beta = 0.0;
    progress.initial = 0.0; progress.residual = 0.0;
    for (var i = 0u; i < params.count; i++) {
        if !finite(water[i]) { progress.status = 5u; progress.failed = i; return; }
        if !wet(i) { continue; }
        if !finite(source[i]) || any(links[i] < vec4(0.0)) || !all(abs(links[i]) <= vec4(3.402823466e38)) {
            progress.status = 5u; progress.failed = i; return;
        }
        for (var a = 0u; a < 3u; a++) {
            let j = neighbor(i, a, true);
            if !wet(j) || links[i][a] <= 0.0 { continue; }
            let r = root(labels[i]); let s = root(labels[j]);
            pockets[max(r,s)] = min(r,s);
        }
    }
    for (var i = 0u; i < params.count; i++) { if is_root(i) { pockets[i] = root(i); } }
    for (var i = 0u; i < params.count; i++) {
        if !wet(i) { continue; }
        let r = pockets[labels[i]];
        let value = vec4(source[i], abs(source[i]), links[i].w, 0.0);
        let y = value - compensation[r]; let t = sums[r] + y;
        compensation[r] = (t - sums[r]) - y; sums[r] = t;
    }
    for (var i = 0u; i < params.count; i++) {
        if !is_root(i) || pockets[i] != i || sums[i].z > 0.0 { continue; }
        // f32 input/gather roundoff only. Never subtract this mismatch.
        if abs(sums[i].x) > 8.0 * EPSILON * sums[i].y {
            progress.status = 3u; progress.failed = i; return;
        }
    }
}
fn gauge(i: u32) -> bool { return pockets[i] == i && sums[i].z == 0.0; }
fn value(i: u32, solution: bool) -> f32 {
    if solution { return pressure[i]; } return vectors[i].y;
}
fn product(i: u32, solution: bool) -> f32 {
    let base = (xyz(i, params.n) / 2u) * 2u;
    let p = value(i, solution);
    var result = 0.0;
    for (var k = 0u; k < 8u; k++) {
        let j = child(base, k); if j == NONE { continue; }
        if labels[j] != i { continue; }
        result += 0.5 * links[j].w * p;
        for (var a = 0u; a < 3u; a++) {
            for (var side = 0u; side < 2u; side++) {
                let q = neighbor(j, a, side == 1u);
                if !wet(q) { continue; }
                let r = labels[q]; if r == i { continue; }
                let owner = select(q, j, side == 1u);
                result += 0.5 * links[owner][a] * (p - value(r, solution));
            }
        }
    }
    return result;
}
@compute @workgroup_size(1)
fn init_main() {
    if progress.status != 0u { return; }
    var rr = 0.0; var largest = 0.0;
    for (var i = 0u; i < params.count; i++) {
        if !is_root(i) { continue; }
        largest = max(largest, abs(rhs[i]));
        if gauge(i) { continue; }
        let r = rhs[i]; vectors[i] = vec4(r, r, 0.0, 0.0); rr += r*r;
    }
    progress.initial = largest; progress.rr = rr;
    if !finite(rr) || !finite(largest) { progress.status = 4u; return; }
    if rr == 0.0 { progress.status = 1u; }
}
@compute @workgroup_size(256)
fn apply_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count || progress.status != 0u { return; }
    if !is_root(i) || gauge(i) { return; }
    // beta was reduced after the preceding update; do not update directions
    // in this dispatch, since another row reads them.
    vectors[i].z = product(i, false);
}
@compute @workgroup_size(1)
fn alpha_main() {
    if progress.status != 0u { return; }
    var dot = 0.0;
    for (var i = 0u; i < params.count; i++) { if is_root(i) { dot += vectors[i].y * vectors[i].z; } }
    if !(dot > 0.0) || !finite(dot) || !finite(progress.rr) {
        progress.status = 4u; return;
    }
    progress.alpha = progress.rr / dot;
}
@compute @workgroup_size(256)
fn update_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count || progress.status != 0u { return; }
    if !is_root(i) || gauge(i) { return; }
    pressure[i] += progress.alpha * vectors[i].y;
    vectors[i].x -= progress.alpha * vectors[i].z;
}
@compute @workgroup_size(1)
fn beta_main() {
    if progress.status != 0u { return; }
    var rr = 0.0; var largest = 0.0;
    for (var i = 0u; i < params.count; i++) {
        if !is_root(i) { continue; }
        let r = vectors[i].x; rr += r*r; largest = max(largest, abs(r));
    }
    progress.iterations += 1u;
    if !finite(rr) { progress.status = 4u; return; }
    if largest <= params.tolerance * progress.initial { progress.status = 1u; return; }
    let beta = rr / progress.rr; progress.beta = beta; progress.rr = rr;
    for (var i = 0u; i < params.count; i++) {
        if is_root(i) { vectors[i].y = vectors[i].x + beta * vectors[i].y; }
    }
}
@compute @workgroup_size(1)
fn finish_main() {
    if progress.status == 0u { progress.status = 2u; }
    if progress.status > 2u { return; }
    var worst = 0.0;
    for (var i = 0u; i < params.count; i++) {
        if is_root(i) { worst = max(worst, abs(rhs[i] - product(i, true))); }
    }
    progress.residual = worst;
}
@compute @workgroup_size(256)
fn transfer_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x; if i >= params.count { return; }
    transfer[i] = vec4(0.0);
    if progress.status > 2u {
        transfer[i] = vec4(bitcast<f32>(0x7fc00000u));
        pressure[i] = bitcast<f32>(0x7fc00000u); return;
    }
    if !wet(i) { return; }
    let r = labels[i]; let p = pressure[r];
    var f = vec4(0.0, 0.0, 0.0, links[i].w * p * 0.5);
    for (var a = 0u; a < 3u; a++) {
        let j = neighbor(i, a, true); if !wet(j) { continue; }
        if labels[j] != r { f[a] = links[i][a] * (p - pressure[labels[j]]) * 0.5; }
    }
    transfer[i] = f;
}

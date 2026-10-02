// GPU FLIP's pressure solver (gpu_flip_pressure.rs): the multigrid-
// preconditioned conjugate gradient for L p = f on the water, every pass a
// separate entry point. Lattices are x-fastest, cell (i, j, k) at
// i + nx·(j + ny·k). A face grid is padded to (n + 1)³ records; weight[a] is
// the open fraction of a cell's low face along axis a, box walls 0.
//
// A level halves each side rounding up. An odd side's extra coarse half is a
// virtual solid cell: it never makes a coarse cell air, its faces count 0,
// and the transfers drop its row, so restriction stays the transpose of
// prolongation over 8 and the V-cycle stays symmetric
// (scripts/mgpcg_reference.py --symmetry).
//
// The free surface is ghost fluid on the finest level only (`ghost` 1): an
// air neighbour a of water cell c adds −w·θ to c's diagonal, θ =
// clamp(φ_a / φ_c, −25, 25) with φ_c taken at most −0.005h and φ_a at least
// 0 (FLIP Fluids pressuresolver.cpp, MIT, Copyright (C) 2026 Ryan L. Guy &
// Dennis Fassbaender; see THIRD_PARTY_NOTICES.md). θ ≤ 0, so a cell's
// diagonal is positive exactly when one of its faces is open, and the
// operator stays symmetric. Coarse levels and the density solve run zero φ:
// the plain rows.
//
// The CPU sizes every buffer for the lattice before it dispatches; each pass
// only checks its thread is inside the lattice.

struct Params {
    // This level's cells: the fine level of a transfer or coarsening.
    nx: u32,
    ny: u32,
    nz: u32,
    // The sweep's color, or the dot product's partial count.
    color: u32,
    // The coarse level's cells.
    cx: u32,
    cy: u32,
    cz: u32,
    // smooth: 1 starts from zero. residual: 1 has no rhs (−L value).
    mode: u32,
    cell_size: f32,
    // The conjugate gradient iteration, or the scalar a dot product writes.
    slot: u32,
    // smooth and residual: 1 reads φ (the finest level's ghost rows).
    ghost: u32,
    // check: the stop's relative tolerance; below 0 the solve never stops early.
    tolerance: f32,
};

struct FaceSample {
    velocity: vec4<f32>,
    weight: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> water: array<f32>;
@group(0) @binding(2) var<storage, read> faces: array<FaceSample>;
@group(0) @binding(3) var<storage, read> src: array<f32>;
@group(0) @binding(4) var<storage, read> aux: array<f32>;
@group(0) @binding(5) var<storage, read_write> out: array<f32>;
@group(0) @binding(6) var<storage, read_write> out2: array<f32>;
@group(0) @binding(7) var<storage, read_write> scalars: array<f32>;
@group(0) @binding(8) var<storage, read> coarse_water: array<f32>;
@group(0) @binding(9) var<storage, read_write> out_faces: array<FaceSample>;
@group(0) @binding(10) var<storage, read> phi: array<f32>;
@group(0) @binding(11) var<storage, read_write> progress: array<f32>;
@group(0) @binding(12) var<storage, read_write> gate: array<u32>;
@group(0) @binding(13) var<storage, read_write> tally: array<u32>;

const DIVISOR_FLOOR: f32 = 1e-30;

var<workgroup> sums: array<f32, 256>;

fn lattice() -> vec3<i32> {
    return vec3<i32>(i32(u.nx), i32(u.ny), i32(u.nz));
}

fn coarse_lattice() -> vec3<i32> {
    return vec3<i32>(i32(u.cx), i32(u.cy), i32(u.cz));
}

fn coords(idx: u32, n: vec3<i32>) -> vec3<i32> {
    return vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
}

fn cell(p: vec3<i32>, n: vec3<i32>) -> u32 {
    return u32(p.x + n.x * (p.y + n.y * p.z));
}

fn is_water(at: u32) -> bool {
    return water[at] > 0.5;
}

// The open fraction of the face between p and its neighbour one step along
// axis a in direction d, 0 past the box.
fn face_weight(p: vec3<i32>, a: i32, d: i32, n: vec3<i32>) -> f32 {
    var q = p;
    q[a] = p[a] + d;
    if q[a] < 0 || q[a] >= n[a] {
        return 0.0;
    }
    var face = p;
    face[a] = max(p[a], q[a]);
    return faces[cell(face, n + vec3<i32>(1))].weight[a];
}

// The ghost ratio θ an air neighbour `air` of water cell `own` puts on the
// diagonal as −w·θ; 0 off the finest level.
fn ghost_ratio(own: u32, air: u32) -> f32 {
    if u.ghost == 0u {
        return 0.0;
    }
    let centre = min(phi[own], -0.005 * u.cell_size);
    return clamp(max(phi[air], 0.0) / (centre + 1e-9), -25.0, 25.0);
}

// The diagonal: Σ w over p's faces, less w·θ per air neighbour; and Σ w ·
// value over its water neighbours, reading `value` from `out` (smoothing in
// place), `aux`, or nowhere (mode 2: the diagonal only).
struct Stencil {
    diagonal: f32,
    sum: f32,
};

fn stencil(p: vec3<i32>, n: vec3<i32>, source: u32) -> Stencil {
    var s = Stencil(0.0, 0.0);
    let own = cell(p, n);
    for (var a = 0; a < 3; a = a + 1) {
        for (var d = -1; d <= 1; d = d + 2) {
            let w = face_weight(p, a, d, n);
            if w > 0.0 {
                s.diagonal = s.diagonal + w;
                var q = p;
                q[a] = p[a] + d;
                let at = cell(q, n);
                if is_water(at) {
                    if source == 0u {
                        s.sum = s.sum + w * out[at];
                    } else if source == 1u {
                        s.sum = s.sum + w * aux[at];
                    }
                } else {
                    s.diagonal = s.diagonal - w * ghost_ratio(own, at);
                }
            }
        }
    }
    return s;
}

// One red-black Gauss-Seidel sweep of L e = rhs in place in `out` (e), rhs
// in `src`. A water cell of the swept color becomes
// (Σ w · water neighbours' e − h² · rhs) / diagonal, or 0 with no open face.
// From zero (mode 1) every other cell is written 0 and neighbours read 0.
@compute @workgroup_size(256, 1, 1)
fn smooth_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = lattice();
    let idx = gid.x;
    if idx >= u.nx * u.ny * u.nz {
        return;
    }
    let p = coords(idx, n);
    let swept = is_water(idx) && u32(p.x + p.y + p.z) % 2u == u.color;
    if !swept {
        if u.mode == 1u {
            out[idx] = 0.0;
        }
        return;
    }
    let h2 = u.cell_size * u.cell_size;
    let s = stencil(p, n, select(0u, 2u, u.mode == 1u));
    out[idx] = select(0.0, (s.sum - h2 * src[idx]) / s.diagonal, s.diagonal > 0.0);
}

// rhs − L value in a water cell with an open face, else 0; rhs in `src`
// (0 with mode 1, which makes −L value), value in `aux`.
@compute @workgroup_size(256, 1, 1)
fn residual_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = lattice();
    let idx = gid.x;
    if idx >= u.nx * u.ny * u.nz {
        return;
    }
    var result = 0.0;
    if is_water(idx) {
        let s = stencil(coords(idx, n), n, 1u);
        if s.diagonal > 0.0 {
            let rhs = select(src[idx], 0.0, u.mode == 1u);
            result = rhs - (s.sum - s.diagonal * aux[idx]) / (u.cell_size * u.cell_size);
        }
    }
    out[idx] = result;
}

// The share fine cell f takes from coarse cell c along one axis in
// prolongation: 3/4 from its parent, 1/4 from the parent's neighbour on its
// side, clamped to the coarse lattice.
fn transfer_weight(f: i32, c: i32, coarse: i32) -> f32 {
    let parent = f / 2;
    let other = clamp(select(parent - 1, parent + 1, f % 2 == 1), 0, coarse - 1);
    return select(0.0, 0.75, parent == c) + select(0.0, 0.25, other == c);
}

// One thread per coarse cell: the fine residual in `src` restricted by the
// transpose of prolongation over 8, masked to coarse water. Fine cells past
// an odd side are the virtual solid: not read.
@compute @workgroup_size(256, 1, 1)
fn restrict_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = lattice();
    let c = coarse_lattice();
    let idx = gid.x;
    if idx >= u.cx * u.cy * u.cz {
        return;
    }
    if !(coarse_water[idx] > 0.5) {
        out[idx] = 0.0;
        return;
    }
    let p = coords(idx, c);
    var sum = 0.0;
    for (var dz = -1; dz <= 2; dz = dz + 1) {
        let fz = 2 * p.z + dz;
        if fz < 0 || fz >= n.z {
            continue;
        }
        let wz = transfer_weight(fz, p.z, c.z);
        for (var dy = -1; dy <= 2; dy = dy + 1) {
            let fy = 2 * p.y + dy;
            if fy < 0 || fy >= n.y {
                continue;
            }
            let wy = wz * transfer_weight(fy, p.y, c.y);
            for (var dx = -1; dx <= 2; dx = dx + 1) {
                let fx = 2 * p.x + dx;
                if fx < 0 || fx >= n.x {
                    continue;
                }
                sum = sum + wy * transfer_weight(fx, p.x, c.x) * src[cell(vec3<i32>(fx, fy, fz), n)];
            }
        }
    }
    out[idx] = sum / 8.0;
}

// One thread per fine cell: in a water cell, e in `out` plus the coarse
// correction in `src` interpolated trilinearly. A fine cell's parent is
// always inside the coarse lattice, odd sides included.
@compute @workgroup_size(256, 1, 1)
fn prolong_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = lattice();
    let c = coarse_lattice();
    let idx = gid.x;
    if idx >= u.nx * u.ny * u.nz || !is_water(idx) {
        return;
    }
    let f = coords(idx, n);
    let parent = f / 2;
    let odd = (f % vec3<i32>(2)) == vec3<i32>(1);
    let other = clamp(select(parent - vec3<i32>(1), parent + vec3<i32>(1), odd), vec3<i32>(0), c - vec3<i32>(1));
    var sum = 0.0;
    for (var corner = 0; corner < 8; corner = corner + 1) {
        let pick = vec3<bool>((corner & 1) != 0, (corner & 2) != 0, (corner & 4) != 0);
        let w = select(vec3<f32>(0.75), vec3<f32>(0.25), pick);
        sum = sum + w.x * w.y * w.z * src[cell(select(parent, other, pick), c)];
    }
    out[idx] = out[idx] + sum;
}

// One thread per coarse cell: water (1) when every fine child inside the
// fine lattice is water, else air (0). A child past an odd side is the
// virtual solid and never makes the cell air.
@compute @workgroup_size(256, 1, 1)
fn coarsen_water_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = lattice();
    let c = coarse_lattice();
    let idx = gid.x;
    if idx >= u.cx * u.cy * u.cz {
        return;
    }
    let p = 2 * coords(idx, c);
    var all_water = true;
    for (var child = 0; child < 8; child = child + 1) {
        let q = p + vec3<i32>(child & 1, (child >> 1u) & 1, (child >> 2u) & 1);
        if all(q < n) && !is_water(cell(q, n)) {
            all_water = false;
        }
    }
    out[idx] = select(0.0, 1.0, all_water);
}

// One thread per padded coarse face record: each coarse face's open fraction
// is the mean of the four fine faces it covers; a fine face past an odd side
// is the virtual solid's and counts 0. Box walls and the padding are 0,
// velocity 0.
@compute @workgroup_size(256, 1, 1)
fn coarsen_faces_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = lattice();
    let c = coarse_lattice();
    let m = c + vec3<i32>(1);
    let idx = gid.x;
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) {
        return;
    }
    var record = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let p = coords(idx, m);
    for (var a = 0; a < 3; a = a + 1) {
        var across = p;
        across[a] = 0;
        if !all(across < c) || p[a] == 0 || p[a] == c[a] {
            continue;
        }
        var sum = 0.0;
        for (var k = 0; k < 4; k = k + 1) {
            var q = 2 * p;
            var bit = 0;
            for (var b = 0; b < 3; b = b + 1) {
                if b != a {
                    q[b] = q[b] + ((k >> u32(bit)) & 1);
                    bit = bit + 1;
                }
            }
            var inside = q;
            inside[a] = 0;
            if all(inside < n) {
                sum = sum + faces[cell(q, n + vec3<i32>(1))].weight[a];
            }
        }
        record.weight[a] = 0.25 * sum;
    }
    out_faces[idx] = record;
}

// The coarsest level's exact solve, e = −h² A⁻¹ rhs: the inverse in `src`
// (cells² row-major, from coarse_inverse.wgsl), rhs in `aux`, e to `out`.
@compute @workgroup_size(64, 1, 1)
fn coarse_solve_main(@builtin(local_invocation_index) i: u32) {
    let cells = u.nx * u.ny * u.nz;
    if i >= cells {
        return;
    }
    var sum = 0.0;
    for (var j = 0u; j < cells; j = j + 1u) {
        sum = sum + src[i * cells + j] * aux[j];
    }
    out[i] = -u.cell_size * u.cell_size * sum;
}

// The conjugate gradient's start: r = f (`src`) on water with an open face,
// else 0, into `out`, and the pressure x = 0 into `out2`, so a solve that
// stops before its first iteration leaves zero pressure. The ghost rows only add to a diagonal, so an open
// face is the whole test.
@compute @workgroup_size(256, 1, 1)
fn init_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = lattice();
    let idx = gid.x;
    if idx >= u.nx * u.ny * u.nz {
        return;
    }
    var diagonal = 0.0;
    if is_water(idx) {
        let p = coords(idx, n);
        for (var a = 0; a < 3; a = a + 1) {
            diagonal = diagonal + face_weight(p, a, -1, n) + face_weight(p, a, 1, n);
        }
    }
    out[idx] = select(0.0, src[idx], diagonal > 0.0);
    out2[idx] = 0.0;
}

// Workgroup g's partial sum of src · aux over a grid stride of the lattice,
// tree-reduced in a fixed order into out2[g]. `color` is the partial count.
@compute @workgroup_size(256, 1, 1)
fn dot_partial_main(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3<u32>) {
    let length = u.nx * u.ny * u.nz;
    var acc = 0.0;
    for (var e = wg.x * 256u + li; e < length; e = e + u.color * 256u) {
        acc = acc + src[e] * aux[e];
    }
    sums[li] = acc;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width = width >> 1u) {
        if li < width {
            sums[li] = sums[li] + sums[li + width];
        }
        workgroupBarrier();
    }
    if li == 0u {
        out2[wg.x] = sums[0];
    }
}

// The partials in order into scalars[slot].
@compute @workgroup_size(1, 1, 1)
fn dot_finalize_main() {
    var total = 0.0;
    for (var g = 0u; g < u.color; g = g + 1u) {
        total = total + out2[g];
    }
    scalars[u.slot] = total;
}

// Iteration k's scalars: rz at 2k, p·s at 2k + 1.
fn ratio(top: f32, bottom: f32) -> f32 {
    return select(0.0, top / bottom, abs(bottom) >= DIVISOR_FLOOR);
}

// p = z (`src`) + β p in `out`, β = rz_k / rz_(k−1); the first iteration
// takes p = z and never reads p.
@compute @workgroup_size(256, 1, 1)
fn direction_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.nx * u.ny * u.nz {
        return;
    }
    let k = u.slot;
    if k == 0u {
        out[idx] = src[idx];
        return;
    }
    let beta = ratio(scalars[2u * k], scalars[2u * k - 2u]);
    out[idx] = src[idx] + beta * out[idx];
}

// x −= α p into `out`, r −= α s into `out2`, α = rz_k / (p·s)_k; p in `src`,
// s in `aux`. The first iteration starts x from zero and never reads it.
@compute @workgroup_size(256, 1, 1)
fn update_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.nx * u.ny * u.nz {
        return;
    }
    let k = u.slot;
    let alpha = ratio(scalars[2u * k], scalars[2u * k + 1u]);
    let x = select(out[idx], 0.0, k == 0u);
    out[idx] = x - alpha * src[idx];
    out2[idx] = out2[idx] - alpha * aux[idx];
}

// The stop, ported from FLIP Fluids (pcgsolver.h solveWithAdditionalMatrix,
// pressuresolver.cpp; MIT, see THIRD_PARTY_NOTICES.md): the residual's
// infinity norm |r|∞ in the right-hand side's units (the divergence, 1/s).
// A solve stops before its first iteration when |f|∞ is under START_FLOOR,
// and after iteration k when |r|∞ ≤ min(tolerance · |f|∞, ACCEPTABLE).
// progress: [0] |f|∞, [1] iterations run, [2] 1 once stopped by the
// tolerance, [4 + k] |r|∞ after iteration k. Stopping zeroes every gate
// triple (`cx` of them), so the solve's later dispatches run no groups.
const START_FLOOR: f32 = 1e-9;
const ACCEPTABLE: f32 = 1.0;

// Workgroup g's max of |src| over a grid stride, into out2[g].
@compute @workgroup_size(256, 1, 1)
fn norm_partial_main(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3<u32>) {
    let length = u.nx * u.ny * u.nz;
    var acc = 0.0;
    for (var e = wg.x * 256u + li; e < length; e = e + u.color * 256u) {
        acc = max(acc, abs(src[e]));
    }
    sums[li] = acc;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width = width >> 1u) {
        if li < width {
            sums[li] = max(sums[li], sums[li + width]);
        }
        workgroupBarrier();
    }
    if li == 0u {
        out2[wg.x] = sums[0];
    }
}

fn stop() {
    progress[2] = 1.0;
    for (var t = 0u; t < u.cx; t = t + 1u) {
        gate[3u * t] = 0u;
    }
}

// The partials' max; mode 1 is the start (|f|∞), else iteration `slot`'s
// |r|∞ and the stop test.
@compute @workgroup_size(1, 1, 1)
fn check_main() {
    var norm = 0.0;
    for (var g = 0u; g < u.color; g = g + 1u) {
        norm = max(norm, out2[g]);
    }
    if u.mode == 1u {
        progress[0] = norm;
        progress[1] = 0.0;
        progress[2] = 0.0;
        if u.tolerance >= 0.0 && norm < START_FLOOR {
            stop();
        }
        return;
    }
    progress[4u + u.slot] = norm;
    progress[1] = f32(u.slot + 1u);
    if u.tolerance >= 0.0 && norm <= min(u.tolerance * progress[0], ACCEPTABLE) {
        stop();
    }
}

// The tick's solver words, after the step's capped records: [0] pressure
// iterations, [1] density iterations, [2] solves that reached their cap
// without meeting the tolerance. Mode 1 clears them first; `slot` is 0 or 1.
@compute @workgroup_size(1, 1, 1)
fn tally_main() {
    if u.mode == 1u {
        tally[0] = 0u;
        tally[1] = 0u;
        tally[2] = 0u;
    }
    tally[u.slot] = tally[u.slot] + u32(progress[1]);
    if u.tolerance >= 0.0 && progress[2] < 0.5 {
        tally[2] = tally[2] + 1u;
    }
}

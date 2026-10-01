// node.coarse_inverse — the inverse of the masked Poisson matrix on the
// multigrid's coarsest level, in one workgroup. A is the cells × cells matrix
// with A[i][i] = Σ w over i's faces and A[i][j] = −w for a water neighbour j
// across a face of open fraction w (node.solid_faces' face grid in
// `solid_faces`; box walls 0), on water cells only (L = −A / h²). `out` is built as A in
// place, then swept one cell at a time (Goodnight's sweep, no pivoting: A is
// symmetric positive definite on water that touches air), which leaves −A⁻¹;
// the last pass writes ½(M + Mᵀ) negated, so the result is A⁻¹ and exactly
// symmetric. A cell whose pivot is under PIVOT_FLOOR of its diagonal ends a
// water body with no air, or a water cell with no open face: it is pinned at zero, its row and column cleared,
// so that body's constant mode is dropped rather than divided by zero. Air
// rows and columns are zero. Works in `out` itself; storageBarrier orders
// the passes. The CPU refuses a lattice past MAX_CELLS or past the arrays
// before dispatch; a lattice that gets here anyway writes nothing.

const MAX_CELLS: u32 = 64u;
const THREADS: u32 = 256u;
const PIVOT_FLOOR: f32 = 1e-4;

struct Params {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    _pad0: u32,
};

@group(0) @binding(0) var<uniform> u: Params;
struct FaceSample {
    velocity: vec4<f32>,
    weight: vec4<f32>,
};

@group(0) @binding(1) var<storage, read> water: array<f32>;
@group(0) @binding(2) var<storage, read> solid_faces: array<FaceSample>;
@group(0) @binding(3) var<storage, read_write> out: array<f32>;

fn coords(c: u32) -> vec3<i32> {
    return vec3<i32>(
        i32(c % u.nodes_x),
        i32((c / u.nodes_x) % u.nodes_y),
        i32(c / (u.nodes_x * u.nodes_y)),
    );
}

// The open fraction of the face between c and its neighbour d steps along
// axis a, or 0 past the box.
fn face_weight(c: u32, a: i32, d: i32) -> f32 {
    let p = coords(c);
    let n = vec3<i32>(i32(u.nodes_x), i32(u.nodes_y), i32(u.nodes_z));
    let m = n + vec3<i32>(1);
    var q = p;
    q[a] = p[a] + d;
    if q[a] < 0 || q[a] >= n[a] {
        return 0.0;
    }
    var face = p;
    face[a] = max(p[a], q[a]);
    return solid_faces[u32(face.x + m.x * (face.y + m.y * face.z))].weight[a];
}

// Σ w over c's faces: A's diagonal on water.
fn diagonal(c: u32) -> f32 {
    var sum = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        sum = sum + face_weight(c, a, -1) + face_weight(c, a, 1);
    }
    return sum;
}

fn entry(i: u32, j: u32) -> f32 {
    if water[i] <= 0.5 || water[j] <= 0.5 {
        return 0.0;
    }
    if i == j {
        return diagonal(i);
    }
    let d = coords(j) - coords(i);
    let steps = abs(d);
    if steps.x + steps.y + steps.z != 1 {
        return 0.0;
    }
    let a = select(select(2, 1, steps.y == 1), 0, steps.x == 1);
    return -face_weight(i, a, d[a]);
}

@compute @workgroup_size(256, 1, 1)
fn inverse_main(@builtin(local_invocation_index) t: u32) {
    let n = u.nodes_x * u.nodes_y * u.nodes_z;
    let entries = n * n;
    if n > MAX_CELLS || n > arrayLength(&water) || entries > arrayLength(&out)
        || (u.nodes_x + 1u) * (u.nodes_y + 1u) * (u.nodes_z + 1u) > arrayLength(&solid_faces) {
        return;
    }
    for (var e = t; e < entries; e = e + THREADS) {
        out[e] = entry(e / n, e % n);
    }
    storageBarrier();
    for (var k = 0u; k < n; k = k + 1u) {
        // Row and column k are only written after the barrier below, so
        // every thread reads the same d. Barriers stay out of branches.
        let d = out[k * n + k];
        let pinned = !(d > PIVOT_FLOOR * diagonal(k));
        for (var e = t; e < entries; e = e + THREADS) {
            let i = e / n;
            let j = e % n;
            if !pinned && i != k && j != k {
                out[e] = out[e] - out[i * n + k] * out[k * n + j] / d;
            }
        }
        storageBarrier();
        for (var e = t; e < entries; e = e + THREADS) {
            let i = e / n;
            let j = e % n;
            if i == k || j == k {
                if pinned {
                    out[e] = 0.0;
                } else if i == j {
                    out[e] = -1.0 / d;
                } else {
                    out[e] = out[e] / d;
                }
            }
        }
        storageBarrier();
    }
    for (var e = t; e < entries; e = e + THREADS) {
        let i = e / n;
        let j = e % n;
        if i <= j {
            let v = -0.5 * (out[e] + out[j * n + i]);
            out[e] = v;
            out[j * n + i] = v;
        }
    }
}

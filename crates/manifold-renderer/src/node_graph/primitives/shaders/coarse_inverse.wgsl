// node.coarse_inverse — the inverse of the masked Poisson matrix on the
// multigrid's coarsest level, in one workgroup. A is the cells × cells matrix
// with A[i][i] = i's neighbours inside the box and A[i][j] = −1 for a water
// neighbour j, on water cells only (L = −A / h²). `out` is built as A in
// place, then swept one cell at a time (Goodnight's sweep, no pivoting: A is
// symmetric positive definite on water that touches air), which leaves −A⁻¹;
// the last pass writes ½(M + Mᵀ) negated, so the result is A⁻¹ and exactly
// symmetric. A cell whose pivot is under PIVOT_FLOOR of its diagonal ends a
// water body with no air: it is pinned at zero, its row and column cleared,
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
@group(0) @binding(1) var<storage, read> water: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;

fn coords(c: u32) -> vec3<i32> {
    return vec3<i32>(
        i32(c % u.nodes_x),
        i32((c / u.nodes_x) % u.nodes_y),
        i32(c / (u.nodes_x * u.nodes_y)),
    );
}

// Neighbours of c inside the box.
fn inside(c: u32) -> f32 {
    let p = coords(c);
    let n = vec3<i32>(i32(u.nodes_x), i32(u.nodes_y), i32(u.nodes_z));
    var count = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        count = count + select(0.0, 1.0, p[a] > 0) + select(0.0, 1.0, p[a] + 1 < n[a]);
    }
    return count;
}

fn entry(i: u32, j: u32) -> f32 {
    if water[i] <= 0.5 || water[j] <= 0.5 {
        return 0.0;
    }
    if i == j {
        return inside(i);
    }
    let d = abs(coords(i) - coords(j));
    return select(0.0, -1.0, d.x + d.y + d.z == 1);
}

@compute @workgroup_size(256, 1, 1)
fn inverse_main(@builtin(local_invocation_index) t: u32) {
    let n = u.nodes_x * u.nodes_y * u.nodes_z;
    let entries = n * n;
    if n > MAX_CELLS || n > arrayLength(&water) || entries > arrayLength(&out) {
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
        let pinned = !(d > PIVOT_FLOOR * inside(k));
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

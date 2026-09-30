// node.coarse_pressure_solve — the multigrid pressure solve's coarsest level,
// solved in one workgroup: the lattice lives in workgroup memory, starts at
// zero, and takes `sweeps` red-black Gauss-Seidel sweeps (red, then black),
// then `sweeps` more (black, then red), so the solve is symmetric. A water
// cell of the swept color becomes (Σ water neighbours − h² · rhs) /
// (neighbours inside the box); air holds zero, the box walls are closed.
// The CPU refuses a lattice past MAX_CELLS or past the arrays before
// dispatch; a lattice that gets here anyway writes nothing.

const MAX_CELLS: u32 = 4096u;
const THREADS: u32 = 256u;

struct Params {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    sweeps: u32,
    cell_size: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> water: array<f32>;
@group(0) @binding(2) var<storage, read> rhs: array<f32>;
@group(0) @binding(3) var<storage, read_write> out: array<f32>;

var<workgroup> value: array<f32, 4096>;

fn is_water(c: u32) -> bool {
    return water[c] > 0.5;
}

fn sweep(t: u32, color: u32, cells: u32) {
    let n = vec3<i32>(i32(u.nodes_x), i32(u.nodes_y), i32(u.nodes_z));
    for (var c = t; c < cells; c = c + THREADS) {
        let p = vec3<i32>(
            i32(c % u.nodes_x),
            i32((c / u.nodes_x) % u.nodes_y),
            i32(c / (u.nodes_x * u.nodes_y)),
        );
        if !is_water(c) || u32(p.x + p.y + p.z) % 2u != color {
            continue;
        }
        var sum = 0.0;
        var inside = 0.0;
        for (var a = 0; a < 3; a = a + 1) {
            for (var d = -1; d <= 1; d = d + 2) {
                var q = p;
                q[a] = p[a] + d;
                if q[a] >= 0 && q[a] < n[a] {
                    inside = inside + 1.0;
                    let at = u32(q.x + n.x * (q.y + n.y * q.z));
                    if is_water(at) {
                        sum = sum + value[at];
                    }
                }
            }
        }
        if inside > 0.0 {
            value[c] = (sum - u.cell_size * u.cell_size * rhs[c]) / inside;
        }
    }
}

@compute @workgroup_size(256, 1, 1)
fn solve_main(@builtin(local_invocation_index) t: u32) {
    let cells = u.nodes_x * u.nodes_y * u.nodes_z;
    if cells > MAX_CELLS || cells > arrayLength(&water) || cells > arrayLength(&rhs) || cells > arrayLength(&out) {
        return;
    }
    for (var c = t; c < cells; c = c + THREADS) {
        value[c] = 0.0;
    }
    workgroupBarrier();
    for (var half = 0u; half < 2u; half = half + 1u) {
        for (var s = 0u; s < u.sweeps; s = s + 1u) {
            for (var k = 0u; k < 2u; k = k + 1u) {
                sweep(t, k ^ half, cells);
                workgroupBarrier();
            }
        }
    }
    for (var c = t; c < cells; c = c + THREADS) {
        out[c] = value[c];
    }
}

// node.pressure_smooth — fusable BUFFER body, GATHER. One thread per cell:
// one red-black Gauss-Seidel sweep of the masked Poisson equation L p = rhs.
// A water cell of the swept color ((i + j + k) mod 2 == color) becomes
// (Σ water neighbours' value − h² · rhs) / diag. diag counts the neighbours
// inside the box (the walls are closed) plus, for each air neighbour a,
// −clamp(φ_a / φ_c, −25, 25), φ_c the cell's distance taken at most
// −0.005h and φ_a the neighbour's taken at least 0: the air side's ghost
// pressure is that ratio times the cell's, never the cell's own sign.
// Floored at 0, as the engine does; a zero diag keeps the value. Every
// other cell keeps its value. `water`,
// `phi` and `value` are gathered through buf_water, buf_phi and buf_value; a
// lattice longer than any of them gives 0.
//
// Ported from FLIP Fluids pressuresolver.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md

fn body(idx: u32, count: u32, e_rhs: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32, color: i32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || cells > min(min(arrayLength(&buf_water), arrayLength(&buf_value)), arrayLength(&buf_phi)) {
        return 0.0;
    }
    let own = buf_value[idx];
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    if !(buf_water[idx] > 0.5) || (p.x + p.y + p.z) % 2 != color {
        return own;
    }
    let centre = min(buf_phi[idx], -0.005 * cell_size);
    var sum = 0.0;
    var diag = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        for (var d = -1; d <= 1; d = d + 2) {
            var q = p;
            q[a] = p[a] + d;
            if q[a] >= 0 && q[a] < n[a] {
                diag = diag + 1.0;
                let at = u32(q.x + n.x * (q.y + n.y * q.z));
                if buf_water[at] > 0.5 {
                    sum = sum + buf_value[at];
                } else {
                    diag = diag - clamp(max(buf_phi[at], 0.0) / (centre + 1e-9), -25.0, 25.0);
                }
            }
        }
    }
    diag = max(diag, 0.0);
    if diag == 0.0 {
        return own;
    }
    return (sum - cell_size * cell_size * e_rhs) / diag;
}

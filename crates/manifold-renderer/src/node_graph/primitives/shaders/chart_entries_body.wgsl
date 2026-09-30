// node.chart_entries — fusable BUFFER body, GATHER. One thread per collar
// entry (e_entries = its cell). The outward normal is minus the clamped
// central gradient of `smoothed`. Along each axis a the thread walks the
// cell's line twice: first counting the water runs wholly before the cell
// (it is air, so no run starts at it) and in all, then counting the collar
// cells on the line whose sheet matches the entry's, which is D for that
// view's slot. Sheet of view +a: runs before − 1; of view −a: runs after − 1;
// both clamped to [0, sheets − 1]. `water`, `smoothed` and `collar` are
// gathered through buf_water, buf_smoothed and buf_collar; a lattice longer
// than any of them gives empty entries.

fn chart_entry_cell(q: vec3<i32>, n: vec3<i32>) -> u32 {
    return u32(q.x + n.x * (q.y + n.y * q.z));
}

fn chart_entry_sheet(runs: u32, sheets: u32) -> u32 {
    return min(max(runs, 1u) - 1u, sheets - 1u);
}

fn body(idx: u32, count: u32, e_entries: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, sheets: i32) -> Element {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    let lattice_fits = cells <= min(arrayLength(&buf_water), min(arrayLength(&buf_smoothed), arrayLength(&buf_collar)));
    if e_entries >= cells || !lattice_fits {
        return Element(vec3<f32>(0.0), 0u, vec3<f32>(0.0), 0xffffffffu);
    }
    let nl = u32(clamp(sheets, 1, 16));
    let c = e_entries;
    let p = vec3<i32>(
        i32(c % u32(n.x)),
        i32((c / u32(n.x)) % u32(n.y)),
        i32(c / (u32(n.x) * u32(n.y))),
    );
    var grad = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        var hi = p;
        hi[a] = min(p[a] + 1, n[a] - 1);
        var lo = p;
        lo[a] = max(p[a] - 1, 0);
        grad[a] = 0.5 * (buf_smoothed[chart_entry_cell(hi, n)] - buf_smoothed[chart_entry_cell(lo, n)]);
    }
    let size = length(grad);
    let normal = select(vec3<f32>(0.0), -grad / size, size > 1e-12);
    var plus = max(normal, vec3<f32>(0.0));
    var minus = max(-normal, vec3<f32>(0.0));
    var packed = 0u;
    for (var a = 0; a < 3; a = a + 1) {
        var total = 0u;
        var before = 0u;
        var wet_prev = false;
        for (var t = 0; t < n[a]; t = t + 1) {
            var q = p;
            q[a] = t;
            let wet = buf_water[chart_entry_cell(q, n)] > 0.5;
            if wet && !wet_prev {
                total = total + 1u;
                if t < p[a] {
                    before = before + 1u;
                }
            }
            wet_prev = wet;
        }
        let sheet_plus = chart_entry_sheet(before, nl);
        let sheet_minus = chart_entry_sheet(total - before, nl);
        var runs = 0u;
        var in_plus = 0u;
        var in_minus = 0u;
        wet_prev = false;
        for (var t = 0; t < n[a]; t = t + 1) {
            var q = p;
            q[a] = t;
            let cell = chart_entry_cell(q, n);
            let wet = buf_water[cell] > 0.5;
            if wet && !wet_prev {
                runs = runs + 1u;
            }
            wet_prev = wet;
            if buf_collar[cell] != 0u {
                in_plus = in_plus + u32(chart_entry_sheet(runs, nl) == sheet_plus);
                in_minus = in_minus + u32(chart_entry_sheet(total - runs, nl) == sheet_minus);
            }
        }
        plus[a] = plus[a] / sqrt(f32(max(in_plus, 1u)));
        minus[a] = minus[a] / sqrt(f32(max(in_minus, 1u)));
        packed = packed | ((sheet_plus | (sheet_minus << 4u)) << (8u * u32(a)));
    }
    return Element(plus, packed, minus, c);
}

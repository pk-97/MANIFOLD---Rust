// node.chart_sums — fusable BUFFER body, GATHER. One thread per chart plane
// element: view v = plane / sheets (axis v / 2, + for even v), sheet
// s = plane % sheets, (i, j) its place on the M × M plane in the view's two
// other axes, lower axis first. The thread walks the line along the view's
// axis; a cell is collar where the running total steps up, its entry being
// the total minus one. Sums share × value over the entries whose sheet in
// view v is s. `total`, `entries` and `value` are gathered through
// buf_total, buf_entries and buf_value; entries past either array add
// nothing, and a lattice longer than `total` gives 0.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, sheets: i32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    let nl = u32(clamp(sheets, 1, 16));
    let m = u32(max(max(n.x, n.y), n.z));
    let plane = idx / (m * m);
    let view = plane / nl;
    let sheet = plane % nl;
    if view >= 6u || cells > arrayLength(&buf_total) {
        return 0.0;
    }
    let a = i32(view / 2u);
    let lower = select(0, 1, a == 0);
    let upper = select(2, 1, a == 2);
    let i = i32(idx % m);
    let j = i32((idx / m) % m);
    if i >= n[lower] || j >= n[upper] {
        return 0.0;
    }
    let usable = min(arrayLength(&buf_entries), arrayLength(&buf_value));
    let shift = 4u * view;
    let plus = (view & 1u) == 0u;
    var q = vec3<i32>(0, 0, 0);
    q[lower] = i;
    q[upper] = j;
    var sum = 0.0;
    for (var t = 0; t < n[a]; t = t + 1) {
        q[a] = t;
        let cell = u32(q.x + n.x * (q.y + n.y * q.z));
        let running = buf_total[cell];
        let previous = select(0u, buf_total[max(cell, 1u) - 1u], cell > 0u);
        if running > previous && running - 1u < usable {
            let e = running - 1u;
            let entry = buf_entries[e];
            if ((entry.sheets >> shift) & 15u) == sheet {
                let share = select(entry.view_minus[a], entry.view_plus[a], plus);
                sum = sum + share * buf_value[e];
            }
        }
    }
    return sum;
}

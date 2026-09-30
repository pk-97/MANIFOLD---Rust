// node.chart_spread — fusable BUFFER body, GATHER. One thread per element of
// a collar vector. An entry reads its six chart slots (the plane layout of
// node.chart_sums) weighted by its shares, plus (2 / h) × its own value. The
// element just past the entries is the vector's constant and passes through;
// empty entries and anything further give 0. `entries` and `planes` are
// gathered through buf_entries and buf_planes; slots past `planes` add
// nothing.

fn body(idx: u32, count: u32, e_value: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, sheets: i32, cell_size: f32) -> f32 {
    let k = arrayLength(&buf_entries);
    if idx >= k {
        return select(0.0, e_value, idx == k);
    }
    let entry = buf_entries[idx];
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if entry.cell >= cells {
        return 0.0;
    }
    let nl = u32(clamp(sheets, 1, 16));
    let m = u32(max(max(n.x, n.y), n.z));
    let planes = arrayLength(&buf_planes);
    let p = vec3<i32>(
        i32(entry.cell % u32(n.x)),
        i32((entry.cell / u32(n.x)) % u32(n.y)),
        i32(entry.cell / (u32(n.x) * u32(n.y))),
    );
    var sum = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        let lower = select(0, 1, a == 0);
        let upper = select(2, 1, a == 2);
        let place = u32(p[lower]) + m * u32(p[upper]);
        for (var sign = 0u; sign < 2u; sign = sign + 1u) {
            let view = 2u * u32(a) + sign;
            let sheet = min((entry.sheets >> (4u * view)) & 15u, nl - 1u);
            let slot = (view * nl + sheet) * m * m + place;
            if slot < planes {
                let share = select(entry.view_minus[a], entry.view_plus[a], sign == 0u);
                sum = sum + share * buf_planes[slot];
            }
        }
    }
    return sum + (2.0 / cell_size) * e_value;
}

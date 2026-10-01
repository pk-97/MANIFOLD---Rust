// node.prolong_lattice — fusable BUFFER body, GATHER. One thread per fine
// cell: value plus, in a water cell, the coarse lattice (half as many cells
// per axis) interpolated trilinearly at it: per axis 3/4 from its parent
// coarse cell and 1/4 from the parent's neighbour on its side, clamped at
// the walls. `coarse` is gathered through buf_coarse; a coarse lattice
// longer than it gives value unchanged.

fn body(idx: u32, count: u32, e_value: f32, e_water: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> f32 {
    let m = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let n = m / 2;
    let cells = u32(m.x) * u32(m.y) * u32(m.z);
    let coarse = u32(n.x) * u32(n.y) * u32(n.z);
    // An odd side would put a parent past the coarse lattice.
    let odd_side = any((m % vec3<i32>(2)) != vec3<i32>(0));
    if idx >= cells || odd_side || coarse > arrayLength(&buf_coarse) || any(n < vec3<i32>(1)) || !(e_water > 0.5) {
        return e_value;
    }
    let f = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    let parent = f / 2;
    let odd = (f % vec3<i32>(2)) == vec3<i32>(1);
    let other = clamp(select(parent - vec3<i32>(1), parent + vec3<i32>(1), odd), vec3<i32>(0), n - vec3<i32>(1));
    var sum = 0.0;
    for (var corner = 0; corner < 8; corner = corner + 1) {
        let pick = vec3<bool>((corner & 1) != 0, (corner & 2) != 0, (corner & 4) != 0);
        let c = select(parent, other, pick);
        let w = select(vec3<f32>(0.75), vec3<f32>(0.25), pick);
        sum = sum + w.x * w.y * w.z * buf_coarse[u32(c.x + n.x * (c.y + n.y * c.z))];
    }
    return e_value + sum;
}

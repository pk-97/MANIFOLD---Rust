// node.coarsen_solid_faces — fusable BUFFER body, GATHER. One thread per
// padded cell of the coarse face grid: each coarse face's open fraction is
// the mean of the four fine faces it covers (the fine grid has twice as many
// cells per axis), so a coarse face's area open to water is the fine faces'.
// Box wall faces and faces past the lattice are 0, velocity 0. `fine` is
// gathered through buf_fine; a fine grid longer than it gives zeros.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> Element {
    var out = Element(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let f = 2 * n + vec3<i32>(1);
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) || u32(f.x) * u32(f.y) * u32(f.z) > arrayLength(&buf_fine) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if !all(other < n) || p[a] == 0 || p[a] == n[a] {
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
            sum = sum + buf_fine[u32(q.x + f.x * (q.y + f.y * q.z))].face_weight[a];
        }
        out.face_weight[a] = 0.25 * sum;
    }
    return out;
}

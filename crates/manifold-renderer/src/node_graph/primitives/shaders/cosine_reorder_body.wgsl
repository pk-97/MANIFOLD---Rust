// node.cosine_reorder — fusable BUFFER body, GATHER. The index shuffle that
// turns a cosine transform into a plain FFT (Makhoul 1980), on every
// transformed axis of a lattice (node (i, j, k) at i + nx·(j + ny·k)): x, y
// and z with axes 3, x and y with axes 2 (z slices stay put). Forward:
// v[p] = x[2p] for p < n/2, v[n - 1 - p] = x[2p + 1]. Inverse undoes it.
//
// The lattice may be a window of a whole lattice (outer lengths, 0 = the
// window is the whole lattice) at `origin`. Forward gathers the window out of
// the whole lattice into a compact one; inverse spreads the compact window
// back over the whole lattice, 0 outside it. Any window that leaves the
// whole lattice, or a lattice longer than `values`, gives 0: never a stray
// read.

fn cosine_reorder_source(p: u32, n: u32) -> u32 {
    return select(2u * (n - 1u - p) + 1u, 2u * p, p < n / 2u);
}

fn cosine_reorder_target(m: u32, n: u32) -> u32 {
    return select(n - 1u - (m - 1u) / 2u, m / 2u, (m & 1u) == 0u);
}

// Every length 1 to 4096 and the product at most `len`, checked without
// overflowing u32.
fn cosine_reorder_fits(v: vec3<u32>, len: u32) -> bool {
    if any(v < vec3<u32>(1u)) || any(v > vec3<u32>(4096u)) {
        return false;
    }
    return v.x * v.y <= len / v.z;
}

fn cosine_reorder_shuffle(c: vec3<u32>, n: vec3<u32>, direction: i32, axes: i32) -> vec3<u32> {
    var s = vec3<u32>(
        cosine_reorder_source(c.x, n.x),
        cosine_reorder_source(c.y, n.y),
        cosine_reorder_source(c.z, n.z),
    );
    if direction != 0 {
        s = vec3<u32>(
            cosine_reorder_target(c.x, n.x),
            cosine_reorder_target(c.y, n.y),
            cosine_reorder_target(c.z, n.z),
        );
    }
    if axes == 2 {
        s.z = c.z;
    }
    return s;
}

fn body(
    idx: u32,
    count: u32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    direction: i32,
    axes: i32,
    origin_x: f32,
    origin_y: f32,
    origin_z: f32,
    outer_x: f32,
    outer_y: f32,
    outer_z: f32,
) -> f32 {
    let limit = vec3<f32>(4096.0);
    let n = vec3<u32>(clamp(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0), limit));
    let a = vec3<u32>(clamp(vec3<f32>(origin_x, origin_y, origin_z), vec3<f32>(0.0), limit));
    let wide = vec3<u32>(clamp(vec3<f32>(outer_x, outer_y, outer_z), vec3<f32>(0.0), limit));
    let o = select(wide, n, wide == vec3<u32>(0u));
    let len = arrayLength(&buf_values);
    if !cosine_reorder_fits(n, 4294967295u) || !cosine_reorder_fits(o, 4294967295u) || any(a + n > o) {
        return 0.0;
    }
    if direction == 0 {
        if idx >= n.x * n.y * n.z || !cosine_reorder_fits(o, len) {
            return 0.0;
        }
        let c = vec3<u32>(idx % n.x, (idx / n.x) % n.y, idx / (n.x * n.y));
        let g = a + cosine_reorder_shuffle(c, n, direction, axes);
        return buf_values[g.x + o.x * (g.y + o.y * g.z)];
    }
    if idx >= o.x * o.y * o.z || !cosine_reorder_fits(n, len) {
        return 0.0;
    }
    let g = vec3<u32>(idx % o.x, (idx / o.x) % o.y, idx / (o.x * o.y));
    if any(g < a) || any(g >= a + n) {
        return 0.0;
    }
    let s = cosine_reorder_shuffle(g - a, n, direction, axes);
    return buf_values[s.x + n.x * (s.y + n.y * s.z)];
}

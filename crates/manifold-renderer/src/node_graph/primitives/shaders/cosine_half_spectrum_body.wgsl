// node.cosine_half_spectrum — fusable BUFFER body, GATHER. The inverse of
// node.cosine_spectrum: from DCT-II coefficients X, rebuild the half spectrum
// V of the reordered lattice, ready for node.inverse_fft_3d. Per axis the
// inverse map is V[k] = conj W(k) · (X[k] − i X[N − k]) with X[N] = 0; in 3D
// the three maps multiply out to eight gathers:
//   V[k] = Π_a conj W(k_a) · Σ_{mirror set S} (−i)^|S| X[k mirrored on S].
// One thread per half-spectrum entry (nx/2 + 1 along x). `values` is gathered
// through `buf_values`; the output element is struct Element { x, y }.

fn cosine_half_value(k: vec3<i32>, n: vec3<i32>) -> f32 {
    if k.x == n.x || k.y == n.y || k.z == n.z {
        return 0.0;
    }
    return buf_values[u32(k.x + n.x * (k.y + n.y * k.z))];
}

fn cosine_half_twiddle(k: i32, n: i32) -> vec2<f32> {
    let angle = 1.5707963267948966 * f32(k) / f32(n);
    return vec2<f32>(cos(angle), sin(angle));
}

fn cosine_half_cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> Element {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let hx = n.x / 2 + 1;
    let k = vec3<i32>(
        i32(idx % u32(hx)),
        i32((idx / u32(hx)) % u32(n.y)),
        i32(idx / u32(hx * n.y)),
    );
    // (−i)^0..3 as (re, im).
    let rot = array<vec2<f32>, 4>(
        vec2<f32>(1.0, 0.0), vec2<f32>(0.0, -1.0), vec2<f32>(-1.0, 0.0), vec2<f32>(0.0, 1.0),
    );
    var sum = vec2<f32>(0.0, 0.0);
    for (var s = 0; s < 8; s = s + 1) {
        let mx = s & 1;
        let my = (s >> 1u) & 1;
        let mz = (s >> 2u) & 1;
        let q = vec3<i32>(
            select(k.x, n.x - k.x, mx == 1),
            select(k.y, n.y - k.y, my == 1),
            select(k.z, n.z - k.z, mz == 1),
        );
        sum = sum + rot[mx + my + mz] * cosine_half_value(q, n);
    }
    let w = cosine_half_cmul(
        cosine_half_cmul(cosine_half_twiddle(k.x, n.x), cosine_half_twiddle(k.y, n.y)),
        cosine_half_twiddle(k.z, n.z),
    );
    let v = cosine_half_cmul(w, sum);
    return Element(v.x, v.y);
}

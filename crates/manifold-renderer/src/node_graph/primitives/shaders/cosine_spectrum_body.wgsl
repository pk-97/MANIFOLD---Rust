// node.cosine_spectrum — fusable BUFFER body, GATHER. One thread per lattice
// node k: the unnormalised DCT-II coefficient
//   X[k] = Σ_n x[n] Π_a cos(π k_a (2 n_a + 1) / (2 N_a))
// over the transformed axes, from V, the half spectrum of the reordered
// lattice (node.fft_3d). With W_N(k) = exp(-iπk / 2N) and V real-symmetric,
//   axes 3: X[k] = ¼ Re Σ_{sy, sz = ±1} W(kx) W(sy·ky) W(sz·kz) V[kx, sy·ky, sz·kz]
//   axes 2: X[k] = ½ Re Σ_{sy = ±1} W(kx) W(sy·ky) V[kx, sy·ky, kz]
// V holds nx/2 + 1 entries along x; the rest are conjugate mirrors, mirrored
// on the transformed axes only.
// `spectrum` is gathered through `buf_spectrum` (struct Element { x, y }).

fn cosine_spectrum_at(k: vec3<i32>, n: vec3<i32>, axes: i32) -> vec2<f32> {
    let w = ((k % n) + n) % n;
    let hx = n.x / 2 + 1;
    if w.x < hx {
        let e = buf_spectrum[u32(w.x + hx * (w.y + n.y * w.z))];
        return vec2<f32>(e.x, e.y);
    }
    var m = (n - w) % n;
    if axes == 2 {
        m.z = w.z;
    }
    let e = buf_spectrum[u32(m.x + hx * (m.y + n.y * m.z))];
    return vec2<f32>(e.x, -e.y);
}

fn cosine_twiddle(k: i32, n: i32) -> vec2<f32> {
    let angle = -1.5707963267948966 * f32(k) / f32(n);
    return vec2<f32>(cos(angle), sin(angle));
}

fn cosine_cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, axes: i32) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    // Past the lattice, or a half spectrum shorter than the lattice's: 0.
    if idx >= u32(n.x * n.y * n.z) || u32((n.x / 2 + 1) * n.y * n.z) > arrayLength(&buf_spectrum) {
        return 0.0;
    }
    let k = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / u32(n.x * n.y)),
    );
    let wx = cosine_twiddle(k.x, n.x);
    var sum = 0.0;
    if axes == 2 {
        for (var sy = -1; sy <= 1; sy = sy + 2) {
            let ky = sy * k.y;
            let w = cosine_cmul(wx, cosine_twiddle(ky, n.y));
            sum = sum + cosine_cmul(w, cosine_spectrum_at(vec3<i32>(k.x, ky, k.z), n, axes)).x;
        }
        return 0.5 * sum;
    }
    for (var sy = -1; sy <= 1; sy = sy + 2) {
        for (var sz = -1; sz <= 1; sz = sz + 2) {
            let ky = sy * k.y;
            let kz = sz * k.z;
            let w = cosine_cmul(cosine_cmul(wx, cosine_twiddle(ky, n.y)), cosine_twiddle(kz, n.z));
            sum = sum + cosine_cmul(w, cosine_spectrum_at(vec3<i32>(k.x, ky, kz), n, axes)).x;
        }
    }
    return 0.25 * sum;
}

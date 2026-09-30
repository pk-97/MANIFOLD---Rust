// node.cosine_line — the cosine transform of every line of a lattice along one
// axis, one workgroup per line, the line held in workgroup memory. Makhoul's
// method: reorder on load, complex radix-2 FFT with barriers, twiddle on store.
//   forward (DCT-II):  X[k] = Σ_m x[m] cos(π k (2m + 1) / 2N)
//   inverse (DCT-III): the exact inverse of forward (scaled by 1/N)
// Lattice node (i, j, k) at i + nx·(j + ny·k); line length N = nodes[axis], a
// power of two from 2 to 512.

struct Params {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    axis: u32,
    direction: u32,
    lines: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

const THREADS: u32 = 64u;
const PI: f32 = 3.141592653589793;

var<workgroup> line: array<vec2<f32>, 512>;

fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn reverse_bits(v: u32, bits: u32) -> u32 {
    return reverseBits(v) >> (32u - bits);
}

@compute @workgroup_size(64)
fn cs_main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let line_index = wg.x + wg.y * 65535u;
    // Uniform across the workgroup, so the early return keeps barriers uniform.
    if line_index >= params.lines {
        return;
    }
    let n = vec3<u32>(params.nodes_x, params.nodes_y, params.nodes_z);
    let a = params.axis;
    let len = n[a];
    let bits = firstTrailingBit(len);
    // The two axes that are not `a`, in x, y, z order, name the line.
    var other = vec2<u32>(1u, 2u);
    if a == 1u {
        other = vec2<u32>(0u, 2u);
    } else if a == 2u {
        other = vec2<u32>(0u, 1u);
    }
    let strides = vec3<u32>(1u, n.x, n.x * n.y);
    let u = line_index % n[other.x];
    let v = line_index / n[other.x];
    let base = u * strides[other.x] + v * strides[other.y];
    let stride = strides[a];
    let inverse = params.direction != 0u;

    // Load, bit-reversed for the in-place FFT.
    for (var p = lid.x; p < len; p = p + THREADS) {
        var value = vec2<f32>(0.0, 0.0);
        if !inverse {
            // v[p] = x[2p] (p < N/2), x[2(N − 1 − p) + 1] otherwise.
            let s = select(2u * (len - 1u - p) + 1u, 2u * p, p < len / 2u);
            value = vec2<f32>(src[base + s * stride], 0.0);
        } else {
            // V[k] = conj W(k) · (X[k] − i X[N − k]), X[N] = 0.
            let xk = src[base + p * stride];
            var xm = 0.0;
            if p != 0u {
                xm = src[base + (len - p) * stride];
            }
            let angle = 0.5 * PI * f32(p) / f32(len);
            value = cmul(vec2<f32>(cos(angle), sin(angle)), vec2<f32>(xk, -xm));
        }
        line[reverse_bits(p, bits)] = value;
    }
    workgroupBarrier();

    // Radix-2 butterflies; the inverse transform flips the twiddle sign.
    let sign = select(-1.0, 1.0, inverse);
    for (var half = 1u; half < len; half = half * 2u) {
        for (var b = lid.x; b < len / 2u; b = b + THREADS) {
            let pos = b % half;
            let i0 = (b / half) * 2u * half + pos;
            let i1 = i0 + half;
            let angle = sign * PI * f32(pos) / f32(half);
            let t = cmul(vec2<f32>(cos(angle), sin(angle)), line[i1]);
            let e = line[i0];
            line[i0] = e + t;
            line[i1] = e - t;
        }
        workgroupBarrier();
    }

    for (var p = lid.x; p < len; p = p + THREADS) {
        if !inverse {
            // X[k] = Re(W(k) · V[k]), W(k) = exp(−iπk / 2N).
            let angle = -0.5 * PI * f32(p) / f32(len);
            dst[base + p * stride] = cmul(vec2<f32>(cos(angle), sin(angle)), line[p]).x;
        } else {
            // x[2q] = v[q], x[2q + 1] = v[N − 1 − q], v = IFFT(V) / N.
            let q = select(len - 1u - (p - 1u) / 2u, p / 2u, (p & 1u) == 0u);
            dst[base + p * stride] = line[q].x / f32(len);
        }
    }
}

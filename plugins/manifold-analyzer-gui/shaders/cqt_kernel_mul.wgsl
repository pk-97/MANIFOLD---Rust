// CQT sparse CSR matrix-vector multiply.
//
// Consumes the R2C FFT output of an N_fft-sample audio window (stored as
// Hermitean-packed complex: (N/2+1) pairs of (re, im) float32s) and produces
// one complex value per CQT bin via the per-bin sparse kernel constructed by
// `CqtTransform::new`.
//
// Layout matches the CPU reference in `manifold-analyzer-dsp/src/cqt.rs`:
//   for k in 0..num_bins:
//     acc = 0
//     for idx in row_ptr[k]..row_ptr[k+1]:
//       m   = col_idx[idx]
//       fft = FFT[m]         (reconstructed via conjugate symmetry if m > N/2)
//       acc += fft * coef[idx]
//     output[k] = acc
//
// One thread per CQT bin. Bins have wildly varying row lengths (hundreds of
// entries at low freq, handful at high freq), but 264 threads is tiny by GPU
// standards — thread-divergence cost is far below the CPU alternative. If
// profiling ever calls for it, upgrade to workgroup-parallel reduction per
// row.

struct Uniforms {
    // N_fft — total bin count before conjugate-symmetric folding. The R2C
    // FFT buffer only stores bins 0..N/2; bins above use the symmetry
    // `X[N-m] = conj(X[m])`.
    n_fft: u32,
    num_bins: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
// FFT output — R2C Hermitean packing, length (N/2+1) complex = (N/2+1)*2 floats.
// Interleaved [re0, im0, re1, im1, ...].
@group(0) @binding(1) var<storage, read> fft_output: array<vec2<f32>>;
// CSR sparse kernel matrix.
@group(0) @binding(2) var<storage, read> row_ptr: array<u32>;
@group(0) @binding(3) var<storage, read> col_idx: array<u32>;
@group(0) @binding(4) var<storage, read> coef: array<vec2<f32>>;
// CQT bin output — complex, length num_bins.
@group(0) @binding(5) var<storage, read_write> cqt_output: array<vec2<f32>>;

// Complex multiply: (a.x + i a.y) * (b.x + i b.y)
fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        a.x * b.x - a.y * b.y,
        a.x * b.y + a.y * b.x,
    );
}

// Fetch FFT bin m, reconstructing the negative-frequency half via
// conjugate symmetry of a real-input FFT.
fn fft_bin(m: u32) -> vec2<f32> {
    let n_half_plus_1 = u.n_fft / 2u + 1u;
    if (m < n_half_plus_1) {
        return fft_output[m];
    }
    // Mirror: X[N-m] = conj(X[m])
    let mirror = u.n_fft - m;
    let v = fft_output[mirror];
    return vec2<f32>(v.x, -v.y);
}

@compute @workgroup_size(64)
fn cqt_kernel_mul(@builtin(global_invocation_id) gid: vec3<u32>) {
    let k = gid.x;
    if (k >= u.num_bins) {
        return;
    }

    let lo = row_ptr[k];
    let hi = row_ptr[k + 1u];
    var acc = vec2<f32>(0.0, 0.0);
    for (var idx: u32 = lo; idx < hi; idx = idx + 1u) {
        let m = col_idx[idx];
        let fft = fft_bin(m);
        let c = coef[idx];
        acc = acc + cmul(fft, c);
    }
    cqt_output[k] = acc;
}

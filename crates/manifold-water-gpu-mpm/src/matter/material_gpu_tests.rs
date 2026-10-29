//! `matter_material.wgsl` on the GPU, checked on the CPU: the SVD rebuilds F
//! with proper rotations and sorted stretches, and the return map keeps
//! every particle inside the yield cone, keeps its volume, lands pulled-apart
//! grains on the tip and never raises the elastic energy.

use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

const LIBRARY: &str = include_str!("../primitives/shaders/matter_material.wgsl");

const HARNESS: &str = r"
struct Case { f: mat3x3<f32>, plastic: vec4<f32> }
struct Out {
    u: mat3x3<f32>,
    v: mat3x3<f32>,
    sigma: vec4<f32>,
    kept: vec4<f32>,
    plastic: vec4<f32>,
    f: mat3x3<f32>,
    tau: mat3x3<f32>,
}
@group(0) @binding(0) var<uniform> material: MatterMaterial;
@group(0) @binding(1) var<storage, read> cases: array<Case>;
@group(0) @binding(2) var<storage, read_write> outs: array<Out>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= arrayLength(&cases) {
        return;
    }
    let c = cases[i];
    let svd = matter_svd(c.f);
    var plastic = MatterPlastic(c.plastic.x, c.plastic.y);
    let kept = matter_return_map(matter_log_stretch(svd.sigma), &plastic, material);
    // dt 0: the trial F is the case's own, so the update must agree with
    // the map above.
    let up = matter_update(c.f, mat3x3<f32>(), 0.0, MatterPlastic(c.plastic.x, c.plastic.y), material);
    outs[i] = Out(
        svd.u,
        svd.v,
        vec4<f32>(svd.sigma, 0.0),
        kept,
        vec4<f32>(plastic.volume_correction, up.plastic.volume_correction, up.plastic.strain, up.j),
        up.f,
        up.tau,
    );
}
";

/// Floats per `Case` and per `Out` (each mat3x3 is three padded columns).
const CASE_FLOATS: usize = 16;
const OUT_FLOATS: usize = 60;

/// Sand at the design's starting constants: E = 3.537e5 Pa, ν = 0.3, a 35°
/// Friction Angle.
fn material(cohesion: f32) -> [f32; 8] {
    let (e, nu) = (3.537e5f64, 0.3f64);
    let mu = e / (2.0 * (1.0 + nu));
    let lambda = e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let sin = 35f64.to_radians().sin();
    let alpha = (2.0f64 / 3.0).sqrt() * 2.0 * sin / (3.0 - sin);
    [
        mu as f32,
        lambda as f32,
        alpha as f32,
        cohesion,
        0.05,
        0.0,
        0.0,
        0.0,
    ]
}

/// xorshift32 in [-1, 1).
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }
}

type Mat = [[f64; 3]; 3];

/// Columns of a std430 mat3x3 starting at `at`, as rows-by-columns `m[r][c]`.
fn mat(data: &[f32], at: usize) -> Mat {
    std::array::from_fn(|r| std::array::from_fn(|c| f64::from(data[at + 4 * c + r])))
}

fn mul(a: &Mat, b: &Mat) -> Mat {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

fn transpose(a: &Mat) -> Mat {
    std::array::from_fn(|r| std::array::from_fn(|c| a[c][r]))
}

fn det(a: &Mat) -> f64 {
    a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
}

fn max_diff(a: &Mat, b: &Mat) -> f64 {
    (0..9)
        .map(|i| (a[i / 3][i % 3] - b[i / 3][i % 3]).abs())
        .fold(0.0, f64::max)
}

const IDENTITY: Mat = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Near identity (the common case), general, nearly flat and inverted F.
fn cases(count: usize) -> Vec<[f32; CASE_FLOATS]> {
    let mut noise = Noise(0x9e37_79b9);
    (0..count)
        .map(|i| {
            let mut f: [[f32; 3]; 3] =
                std::array::from_fn(|_| std::array::from_fn(|_| noise.next()));
            match i % 4 {
                0 => {
                    for (r, row) in f.iter_mut().enumerate() {
                        for (c, x) in row.iter_mut().enumerate() {
                            *x = f32::from(u8::from(r == c)) + 0.3 * *x;
                        }
                    }
                }
                1 => {}
                2 => {
                    // Column 2 is a mix of columns 0 and 1, off by 1e-5.
                    let (a, b, e) = (noise.next(), noise.next(), 1e-5 * noise.next());
                    for row in &mut f {
                        row[2] = a * row[0] + b * row[1] + e;
                    }
                }
                _ => {
                    for (r, row) in f.iter_mut().enumerate() {
                        for (c, x) in row.iter_mut().enumerate() {
                            let base = f32::from(u8::from(r == c)) + 0.2 * *x;
                            *x = if c == 0 { -base } else { base };
                        }
                    }
                }
            }
            let mut case = [0.0f32; CASE_FLOATS];
            for c in 0..3 {
                for r in 0..3 {
                    case[4 * c + r] = f[r][c];
                }
            }
            // Volume correction in [0, 0.2), plastic strain in [0, 0.2).
            case[12] = 0.1 * (noise.next() + 1.0);
            case[13] = 0.1 * (noise.next() + 1.0);
            case
        })
        .collect()
}

fn shared(device: &GpuDevice, data: &[f32]) -> GpuBuffer {
    let buffer = device.create_buffer((data.len() * 4) as u64, BufferUse::Upload);
    // SAFETY: a shared buffer of exactly these floats; no GPU work in flight.
    unsafe { buffer.write(0, bytemuck::cast_slice(data)) };
    buffer
}

fn run(
    device: &GpuDevice,
    pipeline: &GpuComputePipeline,
    material: &[f32; 8],
    cases: &[[f32; CASE_FLOATS]],
) -> Vec<f32> {
    let input = shared(device, bytemuck::cast_slice(cases));
    let out = device.create_buffer((cases.len() * OUT_FLOATS * 4) as u64, BufferUse::Upload);
    let mut enc = device.create_encoder("matter_material proof");
    enc.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::cast_slice(material),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &input,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &out,
                offset: 0,
            },
        ],
        [(cases.len() as u32).div_ceil(64), 1, 1],
        "matter_material proof",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let ptr = out.mapped_ptr().expect("a shared buffer");
    let bytes = cases.len() * OUT_FLOATS * 4;
    // SAFETY: a shared buffer of OUT_FLOATS f32 per case; its GPU work has completed.
    bytemuck::cast_slice(unsafe { std::slice::from_raw_parts(ptr, bytes) }).to_vec()
}

#[test]
fn svd_and_return_map_hold_their_contracts() {
    let device = manifold_gpu::testkit::test_device();
    let shader = format!("{LIBRARY}\n{HARNESS}");
    let pipeline = device.create_compute_pipeline(&shader, "main", "matter_material proof");
    let cases = cases(4096);
    for cohesion in [0.0f32, 0.02] {
        let m = material(cohesion);
        let [mu, lambda, alpha] = [m[0], m[1], m[2]].map(f64::from);
        let k = alpha * (3.0 * lambda + 2.0 * mu) / (2.0 * mu);
        let energy = |e: [f64; 3]| {
            let tr: f64 = e.iter().sum();
            mu * e.iter().map(|x| x * x).sum::<f64>() + 0.5 * lambda * tr * tr
        };
        let out = run(&device, &pipeline, &m, &cases);
        let mut tips = 0;
        for (case, o) in cases.iter().zip(out.chunks_exact(OUT_FLOATS)) {
            let f = mat(case, 0);
            let (u, v) = (mat(o, 0), mat(o, 12));
            let sigma = [24, 25, 26].map(|i| f64::from(o[i]));
            let scale = f.iter().flatten().fold(1.0f64, |a, x| a.max(x.abs()));

            // SVD.
            let sv: Mat = std::array::from_fn(|r| std::array::from_fn(|c| u[r][c] * sigma[c]));
            assert!(
                max_diff(&mul(&sv, &transpose(&v)), &f) < 1e-4 * scale,
                "{case:?}: U Σ Vᵀ"
            );
            for q in [&u, &v] {
                assert!(
                    max_diff(&mul(&transpose(q), q), &IDENTITY) < 1e-4,
                    "{case:?}: orthonormal"
                );
                assert!((det(q) - 1.0).abs() < 1e-4, "{case:?}: a rotation");
            }
            assert!(sigma[0] >= 0.0 && sigma[1] >= 0.0, "{case:?}: {sigma:?}");
            assert!(
                sigma[0] >= sigma[1] - 1e-5 * scale && sigma[1] >= sigma[2].abs() - 1e-5 * scale,
                "{case:?}: sorted {sigma:?}"
            );

            // Return map.
            let (vc, q) = (f64::from(case[12]), f64::from(case[13]));
            let beta = f64::from(cohesion) * (-q / 0.05).exp();
            let eps_c = sigma.map(|s| s.abs().max(1e-4).ln() + vc / 3.0);
            let tr_c: f64 = eps_c.iter().sum();
            let kept = [28, 29, 30].map(|i| f64::from(o[i]));
            let tr: f64 = kept.iter().sum();
            let dev = kept.map(|e| e - tr / 3.0);
            let dev_norm = dev.iter().map(|x| x * x).sum::<f64>().sqrt();
            assert!(
                dev_norm + k * (tr - beta) < 1e-4,
                "{case:?}: outside the cone"
            );
            assert!(
                tr <= beta + 1e-5,
                "{case:?}: more tension than cohesion holds"
            );
            let vc_out = f64::from(o[32]);
            assert!((tr + vc_out - tr_c).abs() < 1e-5, "{case:?}: volume kept");
            assert!(
                energy(kept) <= energy(eps_c) * (1.0 + 1e-5) + 1e-3,
                "{case:?}: energy raised"
            );
            if tr_c >= beta {
                tips += 1;
                assert!(
                    kept.iter().all(|e| (e - beta / 3.0).abs() < 1e-6),
                    "{case:?}: tip"
                );
            }

            // The update agrees with the map, to the rounding the GPU's
            // compiler leaves between two inlined copies of the SVD.
            assert!(
                (o[33] - o[32]).abs() < 1e-5,
                "{case:?}: volume correction {} vs {}",
                o[33],
                o[32]
            );
            let f_out = mat(o, 36);
            let j = f64::from(o[35]);
            assert!((det(&f_out) - j).abs() < 1e-4 * j.max(1.0), "{case:?}: J");
            assert!(
                (j - tr.exp()).abs() < 1e-4 * j.max(1.0),
                "{case:?}: J from the kept strain"
            );
            let tau = mat(o, 48);
            assert!(
                max_diff(&tau, &transpose(&tau)) < 1e-3 * (mu + lambda),
                "{case:?}: τ symmetric"
            );
            let trace = tau[0][0] + tau[1][1] + tau[2][2];
            assert!(
                (trace - (2.0 * mu + 3.0 * lambda) * tr).abs() < 1e-4 * (mu + lambda),
                "{case:?}: τ trace"
            );
        }
        // Both branches ran: the volume correction pulls some cases apart.
        assert!(
            tips > 0 && tips < cases.len(),
            "cohesion {cohesion}: {tips} tips"
        );
    }
}

//! Test-only MAC projection parity and bounded operator cost.
//! Timings exclude particle transfers, surface rendering, and app overhead.
//! Five samples are a small cost probe, not a frame-pacing benchmark.

use bytemuck::{Pod, Zeroable};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuDevice};
use std::sync::{Arc, OnceLock};
#[path = "../water_projection_reference.rs"]
mod reference;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct U {
    n: u32,
    color: u32,
    h: f32,
    omega: f32,
    sor_min: [u32; 4],
    sor_extent: [u32; 4],
}
fn device() -> &'static Arc<GpuDevice> {
    static D: OnceLock<Arc<GpuDevice>> = OnceLock::new();
    D.get_or_init(|| Arc::new(GpuDevice::new()))
}
fn flags(n: usize, wide: bool) -> Vec<u32> {
    (0..n * n * n)
        .map(|p| {
            let i = p % n;
            let j = (p / n) % n;
            let k = p / (n * n);
            if wide {
                if !(16..48).contains(&i) || !(16..48).contains(&k) || !(1..63).contains(&j) {
                    0
                } else if (1..9).contains(&j) {
                    2
                } else {
                    1
                }
            } else if i == 0 || j == 0 || k == 0 || i + 1 == n || j + 1 == n || k + 1 == n {
                0
            } else if j <= 7 {
                2
            } else {
                1
            }
        })
        .collect()
}
fn read4(b: &GpuBuffer, n: usize) -> Vec<[f32; 4]> {
    let p = b.mapped_ptr().unwrap();
    unsafe { bytemuck::cast_slice(std::slice::from_raw_parts(p, n * n * n * 16)).to_vec() }
}
fn read1(b: &GpuBuffer, n: usize) -> Vec<f32> {
    let p = b.mapped_ptr().unwrap();
    unsafe { bytemuck::cast_slice(std::slice::from_raw_parts(p, n * n * n * 4)).to_vec() }
}
fn n64_faces(n: usize) -> Vec<[f32; 4]> {
    let f = flags(n, true);
    let mut v = vec![[0.; 4]; n * n * n];
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                let p = (k * n + j) * n + i;
                let lower = if j > 0 { f[(k * n + j - 1) * n + i] } else { 0 };
                if f[p] != 0 && lower != 0 && (f[p] == 2 || lower == 2) {
                    v[p][1] = -9.81 / 120.;
                }
            }
        }
    }
    v
}
fn grid_rms(v: &[[f32; 4]], n: usize) -> f64 {
    let f = flags(n, true);
    let mut sum = 0.;
    let mut count = 0.;
    for k in 1..n - 1 {
        for j in 1..9 {
            for i in 1..n - 1 {
                let p = (k * n + j) * n + i;
                let d = ((v[p + 1][0] as f64 - v[p][0] as f64)
                    + (v[p + n][1] as f64 - v[p][1] as f64)
                    + (v[p + n * n][2] as f64 - v[p][2] as f64))
                    / 0.0625;
                if f[p] == 2 {
                    sum += d * d;
                    count += 1.;
                }
            }
        }
    }
    (sum / count).sqrt()
}
struct R {
    n: usize,
    u: U,
    bp: manifold_gpu::GpuComputePipeline,
    dp: manifold_gpu::GpuComputePipeline,
    sp: manifold_gpu::GpuComputePipeline,
    pp: manifold_gpu::GpuComputePipeline,
    faces: GpuBuffer,
    adj: GpuBuffer,
    out: GpuBuffer,
    phi: GpuBuffer,
    rhs: GpuBuffer,
    fl: GpuBuffer,
}
impl R {
    fn new(n: usize, h: f32, fl: &[u32], input: &[[f32; 4]]) -> Self {
        if n == 64 {
            assert!(fl.iter().enumerate().all(|(p, &v)| v != 2 || {
                let i = p % n;
                let j = (p / n) % n;
                let k = p / (n * n);
                (16..48).contains(&i) && (1..9).contains(&j) && (16..48).contains(&k)
            }));
        }
        let d = device();
        let s = include_str!("water_projection.wgsl");
        let c = n * n * n;
        let faces = d.create_buffer_shared((c * 16) as u64);
        let adj = d.create_buffer_shared((c * 16) as u64);
        let out = d.create_buffer_shared((c * 16) as u64);
        let phi = d.create_buffer_shared((c * 4) as u64);
        let rhs = d.create_buffer_shared((c * 4) as u64);
        let fb = d.create_buffer_shared((c * 4) as u64);
        unsafe {
            faces.write(0, bytemuck::cast_slice(input));
            fb.write(0, bytemuck::cast_slice(fl));
        }
        Self {
            n,
            u: U {
                n: n as u32,
                color: 0,
                h,
                omega: 1.7,
                sor_min: if n == 64 {
                    [16, 1, 16, 0]
                } else {
                    [1, 1, 1, 0]
                },
                sor_extent: if n == 64 {
                    [32, 8, 32, 0]
                } else {
                    [14, 7, 14, 0]
                },
            },
            bp: d.create_compute_pipeline(s, "boundary", "wp-b"),
            dp: d.create_compute_pipeline(s, "divergence", "wp-d"),
            sp: d.create_compute_pipeline(s, "sor", "wp-s"),
            pp: d.create_compute_pipeline(s, "project", "wp-p"),
            faces,
            adj,
            out,
            phi,
            rhs,
            fl: fb,
        }
    }
    fn bind(
        &self,
        e: &mut manifold_gpu::GpuEncoder,
        p: &manifold_gpu::GpuComputePipeline,
        u: &U,
        a: &GpuBuffer,
        b: &GpuBuffer,
        name: &str,
        groups: u32,
    ) {
        e.dispatch_compute(
            p,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: a,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &self.fl,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &self.phi,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &self.rhs,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: b,
                    offset: 0,
                },
            ],
            [groups, 1, 1],
            name,
        )
    }
    fn solve(&self, sweeps: usize) -> f64 {
        let started = std::time::Instant::now();
        let mut e = device().create_encoder("water-projection");
        e.clear_buffer(&self.phi);
        e.clear_buffer(&self.rhs);
        let dense = (self.n * self.n * self.n).div_ceil(64) as u32;
        let box_groups =
            (self.u.sor_extent[0] * self.u.sor_extent[1] * self.u.sor_extent[2]).div_ceil(64);
        self.bind(
            &mut e,
            &self.bp,
            &self.u,
            &self.faces,
            &self.adj,
            "boundary",
            dense,
        );
        self.bind(
            &mut e,
            &self.dp,
            &self.u,
            &self.adj,
            &self.out,
            "divergence",
            dense,
        );
        for _ in 0..sweeps {
            for c in 0..2 {
                self.bind(
                    &mut e,
                    &self.sp,
                    &U { color: c, ..self.u },
                    &self.adj,
                    &self.out,
                    "sor",
                    box_groups,
                );
            }
        }
        self.bind(
            &mut e, &self.pp, &self.u, &self.adj, &self.out, "project", dense,
        );
        e.commit_and_wait_completed();
        started.elapsed().as_secs_f64() * 1000.
    }
}
#[test]
fn gpu_projection_matches_cpu_reference() {
    let n = 16;
    let input = reference::fixture();
    let (ex, cp) = reference::project(input.clone());
    let fi: Vec<_> = input
        .iter()
        .map(|x| [x.x as f32, x.y as f32, x.z as f32, 0.])
        .collect();
    let r = R::new(n, 1. / n as f32, &flags(n, false), &fi);
    r.solve(64);
    let g = read4(&r.out, n);
    let gf: Vec<_> = g
        .iter()
        .map(|v| reference::Face {
            x: v[0] as f64,
            y: v[1] as f64,
            z: v[2] as f64,
        })
        .collect();
    assert!(reference::rms(&gf) <= reference::rms(&input) * 0.01);
    let m = g
        .iter()
        .zip(ex)
        .map(|(a, b)| {
            ((a[0] as f64 - b.x).abs())
                .max((a[1] as f64 - b.y).abs())
                .max((a[2] as f64 - b.z).abs())
        })
        .fold(0., f64::max);
    assert!(m <= 1e-4);
    assert!(g.iter().all(|v| v.iter().all(|x| x.is_finite())));
    for (p, v) in g.iter().enumerate() {
        let i = p % n;
        let j = (p / n) % n;
        let k = p / (n * n);
        let solid_x = (i > 0 && reference::cell(i - 1, j, k) == reference::Cell::Solid)
            || reference::cell(i, j, k) == reference::Cell::Solid;
        let solid_y = (j > 0 && reference::cell(i, j - 1, k) == reference::Cell::Solid)
            || reference::cell(i, j, k) == reference::Cell::Solid;
        let solid_z = (k > 0 && reference::cell(i, j, k - 1) == reference::Cell::Solid)
            || reference::cell(i, j, k) == reference::Cell::Solid;
        assert!(
            (!solid_x || v[0].abs() <= 1e-6)
                && (!solid_y || v[1].abs() <= 1e-6)
                && (!solid_z || v[2].abs() <= 1e-6)
        );
    }
    let p = read1(&r.phi, n);
    let e = p
        .iter()
        .zip(cp)
        .map(|(a, b)| (*a as f64 - b).abs())
        .fold(0., f64::max);
    assert!(e <= 1e-5, "phi max abs error {e}");
    assert!(p
        .iter()
        .enumerate()
        .any(
            |(i, x)| reference::cell(i % n, (i / n) % n, i / (n * n)) == reference::Cell::Fluid
                && *x > 0.0
        ));
}
#[test]
fn gpu_projection_zero_input_stays_zero() {
    let n = 16;
    let r = R::new(
        n,
        1. / n as f32,
        &flags(n, false),
        &vec![[0.; 4]; n * n * n],
    );
    r.solve(64);
    assert!(read4(&r.out, n)
        .iter()
        .all(|v| v.iter().all(|x| x.abs() < 1e-7)));
    assert!(read1(&r.phi, n).iter().all(|x| x.abs() < 1e-7))
}

#[test]
fn gpu_projection_n64_cost_probe() {
    let n = 64;
    let input = n64_faces(n);
    let before = grid_rms(&input, n);
    let r = R::new(n, 0.0625, &flags(n, true), &input);
    for _ in 0..3 {
        r.solve(96);
    }
    let mut times = (0..5).map(|_| r.solve(96)).collect::<Vec<_>>();
    times.sort_by(f64::total_cmp);
    let g = read4(&r.out, n);
    assert!(g.iter().all(|v| v.iter().all(|x| x.is_finite())));
    let fl = flags(n, true);
    for (p, v) in g.iter().enumerate() {
        let i = p % n;
        let j = (p / n) % n;
        let k = p / (n * n);
        let sx = fl[p] == 0 || (i > 0 && fl[p - 1] == 0);
        let sy = fl[p] == 0 || (j > 0 && fl[p - n] == 0);
        let sz = fl[p] == 0 || (k > 0 && fl[p - n * n] == 0);
        assert!(
            (!sx || v[0].abs() <= 1e-6)
                && (!sy || v[1].abs() <= 1e-6)
                && (!sz || v[2].abs() <= 1e-6)
        );
    }
    let after = grid_rms(&g, n);
    println!("N64 projection encode-submit-wait median={:.3}ms p95={:.3}ms; fluid divergence RMS {before:.6e} -> {after:.6e} (ratio {:.6e})",times[2],times[4],after/before);
    assert!(after <= before * 0.01);
}

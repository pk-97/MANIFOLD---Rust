//! Native Metal parity proof for MAC APIC transfer stages.
use bytemuck::{Pod, Zeroable};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuDevice};
use std::sync::{Arc, OnceLock};
#[path = "../water_mac_transfer_reference.rs"]
mod reference;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct U {
    n: u32,
    active_count: u32,
    h: f32,
    ox: f32,
    oy: f32,
    oz: f32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct P {
    pm: [f32; 4],
    vd: [f32; 4],
    cx: [f32; 4],
    cy: [f32; 4],
    cz: [f32; 4],
    prev: [f32; 4],
}
fn device() -> &'static Arc<GpuDevice> {
    static D: OnceLock<Arc<GpuDevice>> = OnceLock::new();
    D.get_or_init(|| Arc::new(GpuDevice::new()))
}
struct Harness {
    u: U,
    scatter: manifold_gpu::GpuComputePipeline,
    resolve: manifold_gpu::GpuComputePipeline,
    gather: manifold_gpu::GpuComputePipeline,
    particles: GpuBuffer,
    acc: GpuBuffer,
    status: GpuBuffer,
    grid: GpuBuffer,
}
impl Harness {
    fn new() -> Self {
        let d = device();
        let s = include_str!("water_mac_transfer.wgsl");
        Self {
            u: U {
                n: 16,
                active_count: 0,
                h: 1. / 16.,
                ox: 3.,
                oy: -2.,
                oz: 7.,
            },
            scatter: d.create_compute_pipeline(s, "scatter", "mac-s"),
            resolve: d.create_compute_pipeline(s, "resolve", "mac-r"),
            gather: d.create_compute_pipeline(s, "gather", "mac-g"),
            particles: d.create_buffer_shared((4 * std::mem::size_of::<P>()) as u64),
            acc: d.create_buffer_shared(16 * 16 * 16 * 24),
            status: d.create_buffer_shared(4),
            grid: d.create_buffer_shared(16 * 16 * 16 * 16),
        }
    }
    fn bind(
        &self,
        e: &mut manifold_gpu::GpuEncoder,
        p: &manifold_gpu::GpuComputePipeline,
        name: &str,
        g: u32,
    ) {
        e.dispatch_compute(
            p,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&self.u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &self.acc,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &self.status,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &self.grid,
                    offset: 0,
                },
            ],
            [g, 1, 1],
            name,
        )
    }
    fn run(&mut self, ps: &[P]) -> Vec<P> {
        self.u.active_count = ps.len() as u32;
        unsafe {
            self.particles.write(0, bytemuck::cast_slice(ps));
            self.acc.write(0, &vec![0u8; 16 * 16 * 16 * 24]);
            self.status.write(0, &0u32.to_ne_bytes());
        }
        let mut e = device().create_encoder("water-mac");
        self.bind(&mut e, &self.scatter, "scatter", 1);
        self.bind(&mut e, &self.resolve, "resolve", 64);
        self.bind(&mut e, &self.gather, "gather", 1);
        e.commit_and_wait_completed();
        unsafe {
            bytemuck::cast_slice(std::slice::from_raw_parts(
                self.particles.mapped_ptr().unwrap(),
                4 * std::mem::size_of::<P>(),
            ))
            .to_vec()
        }
    }
}
#[test]
fn water_mac_transfer_native_scatter_resolve_gather() {
    let mut h = Harness::new();
    let ps = [
        P {
            pm: [3.337, -1.709, 7.423, 2.7],
            vd: [1.2, -0.7, 2.4, 0.],
            cx: [0.; 4],
            cy: [0.; 4],
            cz: [0.; 4],
            prev: [9., 8., 7., 0.],
        },
        P {
            pm: [3.411, -1.633, 7.509, 1.3],
            vd: [1.2, -0.7, 2.4, 0.],
            cx: [0.; 4],
            cy: [0.; 4],
            cz: [0.; 4],
            prev: [9., 8., 7., 0.],
        },
        P {
            pm: [3.5, -1.5, 7.5, 0.],
            vd: [4., 5., 6., 42.],
            cx: [1.; 4],
            cy: [2.; 4],
            cz: [3.; 4],
            prev: [9., 8., 7., 6.],
        },
        P {
            pm: [3.6, -1.4, 7.6, 0.],
            vd: [7., 8., 9., 43.],
            cx: [4.; 4],
            cy: [5.; 4],
            cz: [6.; 4],
            prev: [9., 8., 7., 6.],
        },
    ];
    check_fixture(&mut h, &ps);
    let mut affine = ps;
    for particle in &mut affine[..2] {
        particle.cx = [0.2, 0.31, -0.4, 0.0];
        particle.cy = [-0.17, 0.5, 0.23, 0.0];
        particle.cz = [0.61, -0.29, 0.11, 0.0];
        for (axis, row) in [particle.cx, particle.cy, particle.cz].iter().enumerate() {
            particle.vd[axis] = [1.2, -0.7, 2.4][axis]
                + (0..3)
                    .map(|d| row[d] * (particle.pm[d] - [3.0, -2.0, 7.0][d]))
                    .sum::<f32>();
        }
    }
    check_fixture(&mut h, &affine);
    let mut light = affine;
    for particle in &mut light[..2] {
        particle.pm[3] *= 0.01;
    }
    check_fixture(&mut h, &light);
    let mut zero = ps;
    for particle in &mut zero[..2] {
        particle.vd = [0.0; 4];
    }
    check_fixture(&mut h, &zero);
}

fn check_fixture(h: &mut Harness, ps: &[P; 4]) {
    let out = h.run(ps);
    if ps[..2].iter().all(|p| p.vd[..3] == [0.0; 3]) {
        for p in &out[..2] {
            assert_eq!(p.vd[..3], [0.0; 3]);
        }
    }
    let mut cpu = reference::Grid::new([16; 3], 1. / 16., [3., -2., 7.]);
    let inputs: Vec<_> = ps[..2]
        .iter()
        .map(|p| reference::Particle {
            p: std::array::from_fn(|d| f64::from(p.pm[d])),
            v: std::array::from_fn(|d| f64::from(p.vd[d])),
            c: [
                std::array::from_fn(|d| f64::from(p.cx[d])),
                std::array::from_fn(|d| f64::from(p.cy[d])),
                std::array::from_fn(|d| f64::from(p.cz[d])),
            ],
            m: f64::from(p.pm[3]),
        })
        .collect();
    cpu.transfer(&inputs);
    let acc: &[i32] =
        unsafe { std::slice::from_raw_parts(h.acc.mapped_ptr().unwrap().cast(), 16 * 16 * 16 * 6) };
    for cell in 0..16 * 16 * 16 {
        let ijk = [cell % 16, (cell / 16) % 16, cell / 256];
        for axis in 0..3 {
            let mass = cpu.face_mass(axis, ijk);
            let momentum = mass * cpu.face_velocity(axis, ijk);
            // Two rounded Q20 contributions plus f32 scatter arithmetic.
            for (offset, expected) in [(0, mass), (1, momentum)] {
                let actual = f64::from(acc[6 * cell + 2 * axis + offset]) / 1048576.0;
                assert!(
                    (actual - expected).abs() <= 1.1e-5,
                    "cell={cell} axis={axis} offset={offset}: {actual} != {expected}"
                );
            }
        }
    }
    let status = unsafe { std::ptr::read_unaligned(h.status.mapped_ptr().unwrap().cast::<u32>()) };
    assert_eq!(status, 0, "status: {status}");
    let grid: &[[f32; 4]] =
        unsafe { std::slice::from_raw_parts(h.grid.mapped_ptr().unwrap().cast(), 16 * 16 * 16) };
    let mut resolved = reference::Grid::new([16; 3], 1. / 16., [3., -2., 7.]);
    for (cell, face) in grid.iter().enumerate() {
        let ijk = [cell % 16, (cell / 16) % 16, cell / 256];
        for (axis, &value) in face[..3].iter().enumerate() {
            resolved.set_face_velocity(axis, ijk, f64::from(value));
        }
    }
    for (i, input) in inputs.iter().enumerate() {
        let (expected_v, expected_c) = resolved.gather(input);
        let (unquantized_v, _) = cpu.gather(input);
        let max_diff = expected_v
            .iter()
            .zip(unquantized_v)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        println!("GPU gather vs unquantized CPU max abs velocity difference: {max_diff:.6e}");
        for (d, &expected_component) in expected_v.iter().enumerate() {
            assert!(
                (f64::from(out[i].vd[d]) - expected_component).abs() <= 2e-5,
                "v[{i}][{d}] actual={} expected={}",
                out[i].vd[d],
                expected_component
            );
        }
        for (row, expected) in [
            (&out[i].cx, expected_c[0]),
            (&out[i].cy, expected_c[1]),
            (&out[i].cz, expected_c[2]),
        ] {
            for d in 0..3 {
                assert!(
                    (f64::from(row[d]) - expected[d]).abs() <= 1e-3,
                    "C actual={} expected={}",
                    row[d],
                    expected[d]
                );
            }
        }
        assert_eq!(out[i].pm, ps[i].pm);
        assert_eq!(out[i].vd[3], ps[i].vd[3]);
        assert_eq!(out[i].prev, ps[i].prev);
        assert_eq!(out[i].cx[3], ps[i].cx[3]);
        assert_eq!(out[i].cy[3], ps[i].cy[3]);
        assert_eq!(out[i].cz[3], ps[i].cz[3]);
    }
    for i in 2..4 {
        assert_eq!(bytemuck::bytes_of(&out[i]), bytemuck::bytes_of(&ps[i]));
    }
}
